"""Defaults shared by the export and the user interface modules."""

DEFAULTS = {
    # How many items one page or batch holds.
    "page_size": 20,

    # How long a network call may take, in seconds.
    "timeout_s": 30,

    # How many times a failed network call is retried.
    "retries": 3,

    # The currency amounts are shown in.
    "currency": "USD",

    # How dates are written (a strftime format).
    "date_format": "%Y-%m-%d",
}
