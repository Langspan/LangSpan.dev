"""The seeded task families of the coordination benchmark.

Each task is the prompt one agent session receives, plus what the oracle
needs to judge the integrated result: the code units the task owns (to find
duplicate work), the calls and expected results that show it done, and the
fixed edits the stand-in agent makes in a dry run. Nothing here is shown to
an agent except `prompt` and, in the awareness-only arm, peers' `summary`.
"""

import json
import random

FAMILIES = ("rename_vs_new_caller", "contradictory_defaults")
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
    "summary": "renames parse_config in shelf/config.py to load_config and updates every caller",
    "prompt": (
        "Rename the function `parse_config` in `shelf/config.py` to `load_config`, and update "
        "every caller in the `shelf` package to use the new name. Remove the old name "
        "completely: do not keep `parse_config` as an alias or a wrapper."
    ),
    "owns": ["shelf/config.py::parse_config", "shelf/config.py::load_config"],
    "standin": {
        "naive": [{"op": "replace", "paths": list(CONFIG_USERS), "old": OLD_NAME, "new": NEW_NAME, "all": True}],
        "coordinated": [{"op": "replace", "paths": list(CONFIG_USERS), "old": OLD_NAME, "new": NEW_NAME, "all": True}],
    },
}

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
    path = "shelf/features/%s.py" % module
    return {
        "id": "feature-" + module,
        "summary": "adds %s() in %s, which parses the configuration" % (function, path),
        "prompt": (
            "Add a module `%s` with a function `%s(%s)` that returns %s. Parse the configuration "
            "text with the package's existing configuration parser in `shelf/config.py`; do not write "
            "a second parser. Add a unit test for it in `tests/test_%s.py`."
            % (path, function, parameters, returns, module)
        ),
        "owns": [path],
        "module": "shelf.features." + module,
        "function": function,
        "checks": checks,
        "standin": {
            "naive": [{"op": "write", "path": path, "content": feature_source(function, parameters, body, OLD_NAME)}],
            "coordinated": [{"op": "write", "path": path, "content": feature_source(function, parameters, body, NEW_NAME)}],
        },
    }


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
    module, function, value, why = side
    path = "shelf/%s.py" % module
    return {
        "id": "%s-%s" % (key, function),
        "pair": key,
        "summary": "changes how %s() in %s uses the %s default" % (function, path, key),
        "prompt": (
            "%s. `%s()` in `%s` takes this value from the `%s` default in `shelf/settings.py`, "
            "which is %s today. Change that default so that `%s()` uses %s."
            % (why, function, path, key, json.dumps(old), function, json.dumps(value))
        ),
        "owns": ["%s::%s" % (path, function)],
        "function": function,
        "module": "shelf." + module,
        "value": value,
        "standin": {
            "naive": [{"op": "replace", "paths": ["shelf/settings.py"],
                       "old": "%s: %s," % (json.dumps(key), json.dumps(old)),
                       "new": "%s: %s," % (json.dumps(key), json.dumps(value))}],
            "coordinated": [{"op": "replace", "paths": [path],
                             "old": "DEFAULTS[%s]" % json.dumps(key), "new": json.dumps(value)}],
        },
    }


CONTROL = {
    "id": "control-slug",
    "summary": "adds slugify() in shelf/text.py, unrelated to any other task",
    "prompt": (
        "Add a module `shelf/text.py` with a function `slugify(name)` that lowercases `name`, "
        "replaces each run of characters other than letters and digits with one hyphen, and strips "
        "hyphens from both ends. Add a unit test for it in `tests/test_text.py`."
    ),
    "owns": ["shelf/text.py"],
    "module": "shelf.text",
    "function": "slugify",
    "checks": [(["Fresh Pears!"], "fresh-pears"), (["  a--b  "], "a-b")],
    "standin": {
        "naive": [{"op": "write", "path": "shelf/text.py", "content": (
            "import re\n\n\ndef slugify(name):\n"
            "    return re.sub(r\"[^a-z0-9]+\", \"-\", name.lower()).strip(\"-\")\n")}],
    },
}
CONTROL["standin"]["coordinated"] = CONTROL["standin"]["naive"]


def assign(family, sessions, seed):
    """Return one task per session, in session order, chosen by ``seed``.

    The seed picks which features or contradictions run (when N is below
    the pool size) and which session gets which task; the same arguments
    always give the same assignment.
    """
    if family not in FAMILIES:
        raise ValueError("family must be one of " + ", ".join(FAMILIES))
    if sessions not in SESSION_COUNTS:
        raise ValueError("sessions must be one of 2, 5 or 10")
    chooser = random.Random("%s:%d:%d" % (family, sessions, seed))
    if family == "rename_vs_new_caller":
        features = chooser.sample(FEATURES, sessions - 1)
        tasks = [dict(RENAME, role="rename")] + [dict(feature_task(*f), role="new_caller") for f in features]
    else:
        tasks = []
        for key, old, a, b in chooser.sample(CONTRADICTIONS, sessions // 2):
            tasks.append(dict(contradiction_task(key, old, a), role="contradiction"))
            tasks.append(dict(contradiction_task(key, old, b), role="contradiction"))
        if sessions % 2:
            tasks.append(dict(CONTROL, role="control"))
    chooser.shuffle(tasks)
    return tasks
