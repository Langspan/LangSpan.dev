"""Dates the shop works with."""

import datetime


def days_left(day):
    """The number of days from today until ``day``: 0 on the day itself,
    and None once it has passed."""
    left = (day - datetime.date.today()).days
    return left if left >= 0 else None
