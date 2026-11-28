"""The seeded task families of the coordination benchmark.

Each task is what one agent session is asked to do, plus what the oracle
needs to judge the combined result: the code units the task owns (to find
duplicate work), the calls and expected results that show it done, and the
edits the stand-in agent makes in a dry run. A session is shown its own
`task` line and `details`; in the awareness-only arm it is also shown its
peers' `task` and `details`. In the LangSpan arm, LangSpan shares each
session's `task` line, which is why that line names everything a peer
needs to know: the interface or setting the task changes.

The stand-in has three edit lists. `ignore` is what a session does knowing
nothing of its peers. `follow` is what the task's details ask for once the
session knows of the peer work that `trigger` (a regular expression over
the information its arm gave it) recognizes. `hedge` is the edit that holds
up whatever peers do, made without knowing: for a contradiction task, its
value set locally and the shared default kept. A task without a trigger
does the same in every mode.

A task that `leads` changes an interface its peers use: under `--stagger`
it starts first. A task's `reads` are files the stand-in reads before it
chooses its edit, as an agent reads a helper before calling it. When the
information its arm gives matches the trigger only after its edit, a
following stand-in makes the informed edit then, as an agent told of a
break fixes its call.
"""

import json
import random
import re

FAMILIES = ("rename_vs_new_caller", "contradictory_defaults", "discovered_change", "push_contention")
SESSION_COUNTS = (2, 5, 10)

# A configuration every feature check parses: three items, one owner.
SAMPLE = (
    "# A small shop\n"
    "owner = Corner Shop\n"
    "item = apples:12:0.50\n"
    "item = pears:3:0.75\n"
    "item = plums:0:1.20\n"
)

OLD_NAME = "parse_config"
NEW_NAME = "load_config"
CONFIG_USERS = ("shelf/config.py", "shelf/report.py", "shelf/cli.py", "shelf/restock.py")

RENAME = {
    "id": "rename",
    "task": ("Rename the function `parse_config` in `shelf/config.py` to `load_config`, and update "
             "every caller in the `shelf` package to use the new name."),
    "details": "Remove the old name completely: do not keep `parse_config` as an alias or a wrapper.",
    "owns": ["shelf/config.py::parse_config", "shelf/config.py::load_config"],
    "leads": True,
    "standin": {
        "ignore": [{"op": "replace", "paths": list(CONFIG_USERS), "old": OLD_NAME, "new": NEW_NAME, "all": True}],
        "follow": [{"op": "replace", "paths": list(CONFIG_USERS), "old": OLD_NAME, "new": NEW_NAME, "all": True}],
    },
}
RENAME["standin"]["hedge"] = RENAME["standin"]["ignore"]
# Peer work that renames the parser: both names in one line of a peer's task.
RENAME_TRIGGER = r"\b%s\b[^\n]*\b%s\b" % (OLD_NAME, NEW_NAME)

# (module, function, parameters, what it returns, body over `config`, checks)
FEATURES = (
    ("count", "count_items", "text", "the number of items in the configuration",
     "len(config[\"items\"])", [([SAMPLE], 3), (["owner = A\n"], 0)]),
    ("quantity", "total_quantity", "text", "the total quantity of all items",
     "sum(item[\"qty\"] for item in config[\"items\"])", [([SAMPLE], 15)]),
    ("names", "item_names", "text", "the item names, sorted alphabetically",
     "sorted(item[\"name\"] for item in config[\"items\"])", [([SAMPLE], ["apples", "pears", "plums"])]),
    ("stocked", "most_stocked", "text",
     "the name of the item with the largest quantity (the first one on a tie), or None when there are no items",
     "max(config[\"items\"], key=lambda item: item[\"qty\"])[\"name\"] if config[\"items\"] else None",
     [([SAMPLE], "apples"), (["owner = A\n"], None)]),
    ("low", "low_stock", "text, threshold",
     "the names of items whose quantity is below `threshold`, in the order the configuration lists them",
     "[item[\"name\"] for item in config[\"items\"] if item[\"qty\"] < threshold]",
     [([SAMPLE, 4], ["pears", "plums"]), ([SAMPLE, 1], ["plums"])]),
    ("value", "inventory_value", "text",
     "the total value of the stock, quantity times price summed over every item, rounded to two decimals",
     "round(sum(item[\"qty\"] * item[\"price\"] for item in config[\"items\"]), 2)", [([SAMPLE], 8.25)]),
    ("find", "find_item", "text, name",
     "the item dictionary whose name is `name`, or None when there is no such item",
     "next((item for item in config[\"items\"] if item[\"name\"] == name), None)",
     [([SAMPLE, "pears"], {"name": "pears", "qty": 3, "price": 0.75}), ([SAMPLE, "kiwis"], None)]),
    ("owner", "owner_name", "text", "the configured owner, or \"nobody\" when no owner is set",
     "config.get(\"owner\", \"nobody\")", [([SAMPLE], "Corner Shop"), (["item = a:1:1\n"], "nobody")]),
    ("has", "has_item", "text, name", "whether an item named `name` is in the configuration",
     "any(item[\"name\"] == name for item in config[\"items\"])",
     [([SAMPLE, "plums"], True), ([SAMPLE, "kiwis"], False)]),
)


def feature_source(function, parameters, body, parser):
    return "from shelf.config import %s\n\n\ndef %s(%s):\n    config = %s(text)\n    return %s\n" % (
        parser, function, parameters, parser, body)


def feature_task(module, function, parameters, returns, body, checks):
    """A new caller of the parser. No edit holds up whatever a peer does,
    since the oracle refuses any mention of the old name, so its hedge is
    its uninformed edit."""
    path = "shelf/features/%s.py" % module
    uninformed = [{"op": "write", "path": path, "content": feature_source(function, parameters, body, OLD_NAME)}]
    return {
        "id": "feature-" + module,
        "task": "Add a module `%s` with a function `%s(%s)` that returns %s." % (path, function, parameters, returns),
        "details": (
            "Parse the configuration text with the package's configuration parser in `shelf/config.py`; "
            "do not write a second parser. If another session renames that parser, call it by its new "
            "name. Add a unit test for it in `tests/test_%s.py`." % module
        ),
        "owns": [path],
        "module": "shelf.features." + module,
        "function": function,
        "checks": checks,
        "trigger": RENAME_TRIGGER,
        # LangSpan's notice of an interface change in another worktree.
        "notice": r"In flight in worktree[^\n]*\b%s\b" % NEW_NAME,
        "standin": {
            "ignore": uninformed,
            "hedge": uninformed,
            "follow": [{"op": "write", "path": path, "content": feature_source(function, parameters, body, NEW_NAME)}],
        },
    }


def independent_task(*feature):
    """push_contention (issue #611): a feature module that no peer's change
    touches, so the only contention is for `origin main` itself."""
    task = feature_task(*feature)
    del task["trigger"], task["notice"]
    task["details"] = ("Parse the configuration text with `parse_config` in `shelf/config.py`; do not write a "
                       "second parser. Add a unit test for it in `tests/test_%s.py`." % feature[0])
    return dict(task, standin={mode: task["standin"]["ignore"] for mode in ("ignore", "follow", "hedge")})


# (key, old default, side A: module, function, value, why; side B: same)
CONTRADICTIONS = (
    ("page_size", 20,
     ("export", "export_batch", 100, "Warehouse exports are too slow because each request sends too few items. Make each export batch hold 100 items"),
     ("ui", "list_page", 10, "Shop assistants scroll too much on the listing screen. Make each listing page show 10 items")),
    ("timeout_s", 30,
     ("export", "upload_timeout", 120, "Large uploads to the warehouse time out. Give an upload request 120 seconds"),
     ("ui", "health_timeout", 5, "The status light hangs when the server is down. Make the health check give up after 5 seconds")),
    ("retries", 3,
     ("export", "sync_retries", 10, "Warehouse syncs fail on a flaky network. Retry a failed sync 10 times"),
     ("ui", "probe_retries", 0, "The status light is slow to show an outage. Do not retry a failed health check at all")),
    ("currency", "USD",
     ("export", "invoice_total", "EUR", "The warehouse bills in euros. Show invoice totals in EUR"),
     ("ui", "price_label", "GBP", "The shop is in the United Kingdom. Show shelf label prices in GBP")),
    ("date_format", "%Y-%m-%d",
     ("export", "export_filename", "%Y%m%d", "The warehouse system rejects dashes in file names. Write export file dates as %Y%m%d"),
     ("ui", "receipt_date", "%d/%m/%Y", "Customers misread receipt dates. Print receipt dates as %d/%m/%Y")),
)


def contradiction_task(key, old, side):
    """One side of a contradiction. The details define what the oracle
    checks: when a peer needs a different value of the same default, the
    shared default stays as it is and each function gets its own value in
    its own module; when no peer does, the default changes. A run assigns
    a key both its sides or only one, so a session must know its peers'
    tasks to choose."""
    module, function, value, why = side
    path = "shelf/%s.py" % module
    local = [{"op": "replace", "paths": [path], "old": "DEFAULTS[%s]" % json.dumps(key), "new": json.dumps(value)}]
    return {
        "id": "%s-%s" % (key, function),
        "pair": key,
        "task": "Make `%s()` in `%s` use %s as its `%s` setting." % (function, path, json.dumps(value), key),
        "details": (
            "%s. `%s()` reads `%s` from `DEFAULTS` in `shelf/settings.py`, where it is %s today; other "
            "functions read it too. If another session's task sets `%s` to a different value, leave the "
            "default unchanged and give `%s()` the value %s in `%s` itself. Otherwise change the default "
            "to %s."
            % (why, function, key, json.dumps(old), key, function, json.dumps(value), path, json.dumps(value))
        ),
        "owns": ["%s::%s" % (path, function)],
        "function": function,
        "module": "shelf." + module,
        "key": key,
        "old": old,
        "value": value,
        "trigger": r"\b%s\b" % key,
        "standin": {
            "ignore": [{"op": "replace", "paths": ["shelf/settings.py"],
                        "old": "%s: %s," % (json.dumps(key), json.dumps(old)),
                        "new": "%s: %s," % (json.dumps(key), json.dumps(value))}],
            "follow": local,
            "hedge": local,
        },
    }


CONTROL = {
    "id": "control-slug",
    "task": "Add a module `shelf/text.py` with a function `slugify(name)`.",
    "details": (
        "`slugify` lowercases `name`, replaces each run of characters other than letters and digits "
        "with one hyphen, and strips hyphens from both ends. Add a unit test for it in `tests/test_text.py`."
    ),
    "owns": ["shelf/text.py"],
    "module": "shelf.text",
    "function": "slugify",
    "checks": [(["Fresh Pears!"], "fresh-pears"), (["  a--b  "], "a-b")],
    "standin": {
        "ignore": [{"op": "write", "path": "shelf/text.py", "content": (
            "import re\n\n\ndef slugify(name):\n"
            "    return re.sub(r\"[^a-z0-9]+\", \"-\", name.lower()).strip(\"-\")\n")}],
    },
}
CONTROL["standin"]["follow"] = CONTROL["standin"]["hedge"] = CONTROL["standin"]["ignore"]

# discovered_change: no task text names the interface change. The changer's
# task needs `days_left` in `shelf/dates.py` to count from a given day, which
# it cannot: it reads the clock. The changer finds that while working and
# must give it a second, required parameter. `days_left` gives None once a
# day has passed, so no arithmetic on one-argument calls recovers the count
# from a past `today` (issue #218): the change is the only way to finish.
# Each dependent's task calls `days_left` as it is today, so only
# information from the work itself (an interface notice or a break alert)
# can tell it of the change.
CHANGED = "days_left"
CHANGED_ARITY = 2
DATES = "shelf/dates.py"
# A date-labelled sample: each item's best-before date is the given number of
# days after the day the oracle checks against.
DATED_SAMPLE = [["milk", 2], ["bread", -1], ["jam", 30], ["eggs", 0]]
ITEMS_ARE = ("`items` is a list of pairs of an item's name and its best-before date, a `datetime.date`")
# What the oracle checks: the function, and the number of arguments its
# changed form requires.
INTERFACE = {"module": "shelf.dates", "name": CHANGED, "arity": CHANGED_ARITY}

EXPIRY_SOURCE = (
    "from shelf.dates import days_left\n\n\n"
    "def expired(items, today):\n"
    "    return [name for name, day in items if days_left(day, today) is None]\n"
)
CHANGER = {
    "id": "changer-expired",
    "task": ("Add a module `shelf/expiry.py` with a function `expired(items, today)` that returns the names of "
             "the items whose best-before date is before `today`, in the order given."),
    "details": (
        "%s; `today` is a `datetime.date`, any day, past or future. Count the days with `days_left` in `shelf/dates.py`; do not write a "
        "second date helper. Add a unit test for it in `tests/test_expiry.py` that passes whatever day it runs."
        % ITEMS_ARE
    ),
    "owns": ["shelf/dates.py::days_left", "shelf/expiry.py"],
    "module": "shelf.expiry",
    "function": "expired",
    "dates": "fixed",
    "checks": [([DATED_SAMPLE], ["bread"]), ([[["tea", -3], ["rice", 1]]], ["tea"])],
    # The files whose calls of `days_left` the change must update: the
    # package's caller, and the changer's own module.
    "callers": ["shelf/orders.py", "shelf/expiry.py"],
    "interface": INTERFACE,
    "reads": [DATES],
    "leads": True,
}
_CHANGE = [
    {"op": "replace", "paths": [DATES], "old": "def days_left(day):", "new": "def days_left(day, today):"},
    {"op": "replace", "paths": [DATES], "old": "from today until", "new": "from ``today`` until"},
    {"op": "replace", "paths": [DATES], "old": "(day - datetime.date.today()).days", "new": "(day - today).days"},
    {"op": "replace", "paths": ["shelf/orders.py"], "old": "from shelf.dates import days_left\n",
     "new": "import datetime\n\nfrom shelf.dates import days_left\n"},
    {"op": "replace", "paths": ["shelf/orders.py"], "old": "days_left(due)", "new": "days_left(due, datetime.date.today())"},
    {"op": "write", "path": "shelf/expiry.py", "content": EXPIRY_SOURCE},
]
CHANGER["standin"] = {
    "ignore": _CHANGE,
    "follow": _CHANGE,
    # A default for the new parameter keeps every old call working, so the
    # peers need not know: the hedge the success definition rules out.
    "hedge": [
        {"op": "replace", "paths": [DATES], "old": "def days_left(day):", "new": "def days_left(day, today=None):"},
        {"op": "replace", "paths": [DATES], "old": "(day - datetime.date.today()).days",
         "new": "(day - (today or datetime.date.today())).days"},
        {"op": "write", "path": "shelf/expiry.py", "content": EXPIRY_SOURCE},
    ],
    # The third pilot's workaround: two one-argument calls and their
    # difference, with `days_left` left as it is. It cannot find the expired
    # items, so the task fails, and the trial is `change_not_made`.
    "workaround": [{"op": "write", "path": "shelf/expiry.py", "content": (
        "from shelf.dates import days_left\n\n\n"
        "def expired(items, today):\n"
        "    return [name for name, day in items if days_left(day) - days_left(today) < 0]\n")}],
}

# (module, function, parameters, what it returns, body with LEFT(day) where
# it counts the days left, checks over DATED_SAMPLE-like items)
DATED_FEATURES = (
    ("fresh", "fresh_items", "items",
     "the names of the items with at least 3 days left before their best-before date, in the order given",
     "[name for name, day in items if (LEFT(day) or 0) >= 3]", [([DATED_SAMPLE], ["jam"]), ([[]], [])]),
    ("soon", "expiring_soon", "items, within",
     "the names of the items whose best-before date is from today to `within` days away, in the order given",
     "[name for name, day in items if LEFT(day) is not None and LEFT(day) <= within]",
     [([DATED_SAMPLE, 2], ["milk", "eggs"]), ([DATED_SAMPLE, 0], ["eggs"])]),
    ("first", "first_to_expire", "items",
     "the name of the unexpired item with the fewest days left (first on a tie), or None if there is none",
     "min((item for item in items if LEFT(item[1]) is not None), key=lambda item: LEFT(item[1]), "
     "default=(None,))[0]", [([DATED_SAMPLE], "eggs"), ([[]], None)]),
    ("gone", "count_expired", "items", "how many of the items are past their best-before date",
     "sum(1 for name, day in items if LEFT(day) is None)", [([DATED_SAMPLE], 1), ([[]], 0)]),
    ("left", "days_by_name", "items",
     "a dict from each item's name to the days left before its best-before date, or None once it has passed",
     "{name: LEFT(day) for name, day in items}", [([DATED_SAMPLE], {"milk": 2, "bread": None, "jam": 30, "eggs": 0})]),
    ("urgent", "by_urgency", "items",
     "the item names, those past their best-before date first, then by fewest days left, ties in the order given",
     "[name for name, day in sorted(items, key=lambda item: -1 if LEFT(item[1]) is None else LEFT(item[1]))]",
     [([DATED_SAMPLE], ["bread", "eggs", "milk", "jam"])]),
    ("last", "last_to_expire", "items",
     "the name of the item with the most days left (the first one on a tie), or None when there are none",
     "max(items, key=lambda item: -1 if LEFT(item[1]) is None else LEFT(item[1]))[0] if items else None",
     [([DATED_SAMPLE], "jam"), ([[]], None)]),
    ("today", "expires_today", "items",
     "the names of the items whose best-before date is today, in the order given",
     "[name for name, day in items if LEFT(day) == 0]", [([DATED_SAMPLE], ["eggs"])]),
    ("total", "total_days_left", "items",
     "the days left before the best-before date, summed over the items that have not expired",
     "sum(LEFT(day) or 0 for name, day in items)", [([DATED_SAMPLE], 32)]),
)


def dated_source(function, parameters, body, form):
    """A dependent's module. `old` calls `days_left` as the fixture defines
    it; `new` passes today's date as the changed form's second argument;
    `own` counts the days itself, a hedge that works whatever the changer
    does."""
    imports = {"old": "from shelf.dates import days_left\n",
               "new": "import datetime\n\nfrom shelf.dates import days_left\n",
               "own": "import datetime\n"}[form]
    setup = "" if form == "old" else "    today = datetime.date.today()\n"
    call = {"old": "days_left({0})", "new": "days_left({0}, today)",
            "own": "((({0}) - today).days if ({0}) >= today else None)"}[form]
    code = re.sub(r"LEFT\(([^()]*)\)", lambda match: call.format(match.group(1)), body)
    return "%s\n\ndef %s(%s):\n%s    return %s\n" % (imports, function, parameters, setup, code)


def dependent_task(module, function, parameters, returns, body, checks):
    """A new caller of `days_left`. Its text says to call the helper in its
    new form if another session changes it, as the rename family's does,
    and names no change."""
    path = "shelf/features/%s.py" % module
    return {
        "id": "dependent-" + module,
        "task": "Add a module `%s` with a function `%s(%s)` that returns %s." % (path, function, parameters, returns),
        "details": (
            "%s. Count the days with `days_left` in `shelf/dates.py`; do not write a second date helper. If "
            "another session changes how `days_left` is called, call it the new way. Add a unit test for it in "
            "`tests/test_%s.py`." % (ITEMS_ARE, module)
        ),
        "owns": [path],
        "module": "shelf.features." + module,
        "function": function,
        "dates": "today",
        "checks": checks,
        "interface": INTERFACE,
        # The helper and the package's caller of it, as an agent reads how
        # a helper is called before calling it. LangSpan tells of another
        # worktree's change at a file that calls or imports the name; the
        # file defining it does not count.
        "reads": [DATES, "shelf/orders.py"],
        # The changed signature, as LangSpan quotes it in an interface notice
        # or a break alert. No task text holds it.
        "trigger": r"\bdef %s\(day, today\)" % CHANGED,
        # Any LangSpan notice or alert naming the helper.
        "notice": r"(In flight in worktree|calls)[^\n]*\"%s\"" % CHANGED,
        "standin": {
            "ignore": [{"op": "write", "path": path, "content": dated_source(function, parameters, body, "old")}],
            "follow": [{"op": "write", "path": path, "content": dated_source(function, parameters, body, "new")}],
            "hedge": [{"op": "write", "path": path, "content": dated_source(function, parameters, body, "own")}],
        },
    }

# What a successful combined result is, per family. Every prompt, in every
# arm, states it, and the oracle checks exactly this. It is written in terms
# of the tasks, never of what a peer task changes: naming the renamed
# function here would give the baseline arm the peer information that only
# the arm's own channel may carry. The README spells out what it means for
# each family's fixture.
SUCCESS = {
    "rename_vs_new_caller": (
        "every function a task renames exists only under the name that task gives it, with no "
        "alias or wrapper under its old name; every caller in the package, every new feature "
        "module included, calls it by that name and works; and the package's tests pass"
    ),
    "contradictory_defaults": (
        "each function returns the value its task asked for; every shared default in "
        "`shelf/settings.py` that two tasks wanted changed to different values is unchanged; every "
        "shared default only one task wanted changed holds that task's value; and the package's "
        "tests pass"
    ),
    "discovered_change": (
        "every task's function returns what its task asked for; every shared function a task had to change "
        "exists only in its changed form, with no default, alias or wrapper that keeps its old calls working; "
        "every call in the package, every new module included, uses the changed form; and the package's "
        "tests pass"
    ),
    "push_contention": "every task's function returns what its task asked for, and the package's tests pass",
}


def layouts(sessions):
    """The ways to fill SESSIONS with contradiction pairs, keys only one
    task wants (`alone`) and at most one control task, as (pairs, alone,
    controls). Each key appears once per run. From N = 5 every layout has a
    pair and a key alone, so neither fixed edit (always keep the default,
    always change it) can pass."""
    found = []
    for pairs in range(sessions // 2 + 1):
        for controls in (0, 1):
            alone = sessions - 2 * pairs - controls
            if alone < 0 or pairs + alone > len(CONTRADICTIONS):
                continue
            if sessions >= 5 and not (pairs and alone):
                continue
            found.append((pairs, alone, controls))
    return found


def assign(family, sessions, seed):
    """Return one task per session, in session order, chosen by ``seed``.

    The seed picks which features or contradictions run (when N is below
    the pool size), which contradictions get both sides and which one, and
    which session gets which task; the same arguments always give the same
    assignment.
    """
    if family not in FAMILIES:
        raise ValueError("family must be one of " + ", ".join(FAMILIES))
    if sessions not in SESSION_COUNTS:
        raise ValueError("sessions must be one of 2, 5 or 10")
    chooser = random.Random("%s:%d:%d" % (family, sessions, seed))
    if family == "rename_vs_new_caller":
        features = chooser.sample(FEATURES, sessions - 1)
        tasks = [dict(RENAME, role="rename")] + [dict(feature_task(*f), role="new_caller") for f in features]
    elif family == "discovered_change":
        features = chooser.sample(DATED_FEATURES, sessions - 1)
        tasks = [dict(CHANGER, role="changer")] + [dict(dependent_task(*f), role="dependent") for f in features]
    elif family == "push_contention":
        features = chooser.sample(FEATURES, sessions - 1)
        tasks = [dict(independent_task(*f), role="independent") for f in features] + [dict(CONTROL, role="control")]
    else:
        pairs, alone, controls = chooser.choice(layouts(sessions))
        chosen = chooser.sample(CONTRADICTIONS, pairs + alone)
        tasks = []
        for position, (key, old, a, b) in enumerate(chosen):
            sides = (a, b) if position < pairs else (chooser.choice((a, b)),)
            tasks.extend(dict(contradiction_task(key, old, side), role="contradiction") for side in sides)
        tasks.extend(dict(CONTROL, role="control") for _ in range(controls))
    chooser.shuffle(tasks)
    return tasks


def leader(assigned):
    """The session, counted from 1, whose task changes an interface its
    peers use, or None in a family without one."""
    return next((index for index, task in enumerate(assigned, 1) if task.get("leads")), None)
