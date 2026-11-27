"""Compare the repository's GitHub settings with .github/desired-settings.json.

.github/desired-settings.json records the settings and rulesets the owner chose:
merge methods, branch deletion, Actions permissions, the security features
and both rulesets. This script reads the live values with `gh api` (GET
requests only) and prints every difference. It changes nothing. To change a
setting, change it on GitHub and in the file in the same pull request.

The file holds no token or secret value, and the script prints none: the
endpoints it reads return none.

Usage: github-drift.py [--settings FILE] [--observed FILE] [--dump]
  --settings FILE  the desired state (default .github/desired-settings.json)
  --observed FILE  compare with a saved observed state instead of gh api
  --dump           print the observed state in the file's format and exit
Exit status: 0 no drift, 1 drift, 2 the state couldn't be read.
"""

import json
import subprocess
import sys

REPO_FIELDS = [
    "visibility",
    "default_branch",
    "allow_squash_merge",
    "allow_merge_commit",
    "allow_rebase_merge",
    "allow_auto_merge",
    "delete_branch_on_merge",
    "web_commit_signoff_required",
    "has_issues",
    "has_wiki",
    "has_discussions",
    "has_projects",
]
RULESET_FIELDS = ["target", "enforcement", "conditions", "bypass_actors", "rules"]


class ReadError(Exception):
    pass


def gh(path, allow_missing=False):
    """GET one API path; None when allow_missing and GitHub answers 404."""
    out = subprocess.run(
        ["gh", "api", "--method", "GET", path],
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        if allow_missing and "HTTP 404" in out.stderr:
            return None
        raise ReadError(f"gh api {path} failed: {out.stderr.strip()}")
    return json.loads(out.stdout) if out.stdout.strip() else {}


def observe(repo):
    """The live state of `repo`, in the shape of .github/desired-settings.json."""
    base = f"repos/{repo}"
    info = gh(base)
    security = {
        k: v.get("status") for k, v in sorted(info.get("security_and_analysis", {}).items())
    }
    rulesets = {}
    for summary in gh(f"{base}/rulesets"):
        full = gh(f"{base}/rulesets/{summary['id']}")
        entry = {k: full.get(k) for k in RULESET_FIELDS}
        # Keyed by rule type (a ruleset holds each type once), so a
        # difference names the one parameter that changed.
        entry["rules"] = {r["type"]: r.get("parameters", {}) for r in entry["rules"] or []}
        rulesets[full["name"]] = entry
    setup = gh(f"{base}/code-scanning/default-setup")
    return {
        "repository": repo,
        "repo": {k: info.get(k) for k in REPO_FIELDS},
        "security_and_analysis": security,
        "vulnerability_alerts": gh(f"{base}/vulnerability-alerts", allow_missing=True)
        is not None,
        "automated_security_fixes": gh(f"{base}/automated-security-fixes"),
        "private_vulnerability_reporting": gh(f"{base}/private-vulnerability-reporting"),
        "code_scanning_default_setup": {"state": setup.get("state")},
        "actions_permissions": gh(f"{base}/actions/permissions"),
        "workflow_permissions": gh(f"{base}/actions/permissions/workflow"),
        "rulesets": rulesets,
    }


def diff(want, have, path=""):
    """Yield one line per value that differs, keyed by its JSON path."""
    if isinstance(want, dict) and isinstance(have, dict):
        for key in sorted(set(want) | set(have)):
            if key.startswith("_"):
                continue
            where = f"{path}.{key}" if path else key
            if key not in have:
                yield f"{where}: missing on GitHub (want {json.dumps(want[key])})"
            elif key not in want:
                yield f"{where}: not in the settings file (GitHub has {json.dumps(have[key])})"
            else:
                yield from diff(want[key], have[key], where)
    elif want != have:
        yield f"{path}: want {json.dumps(want)}, GitHub has {json.dumps(have)}"


def main(argv):
    settings_path = ".github/desired-settings.json"
    observed_path = None
    dump = False
    args = list(argv)
    while args:
        arg = args.pop(0)
        if arg == "--settings" and args:
            settings_path = args.pop(0)
        elif arg == "--observed" and args:
            observed_path = args.pop(0)
        elif arg == "--dump":
            dump = True
        else:
            print(__doc__, file=sys.stderr)
            return 2
    try:
        with open(settings_path) as f:
            want = json.load(f)
        if observed_path:
            with open(observed_path) as f:
                have = json.load(f)
        else:
            have = observe(want["repository"])
    except (OSError, ValueError, KeyError, ReadError) as e:
        print(f"github-drift: can't read the state: {e}", file=sys.stderr)
        return 2
    if dump:
        print(json.dumps(have, indent=2, sort_keys=True))
        return 0
    lines = list(diff(want, have))
    for line in lines:
        print(line)
    if lines:
        print(
            f"github-drift: {len(lines)} difference(s) from {settings_path}. "
            "Change the setting back, or record the owner's new choice in the file.",
            file=sys.stderr,
        )
        return 1
    print(f"github-drift: GitHub matches {settings_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
