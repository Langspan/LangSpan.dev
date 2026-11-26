"""Dates the shop works with."""

import datetime


def days_left(day):
    """The number of days from today until ``day``: 0 on the day itself,
    negative once it has passed."""
    return (day - datetime.date.today()).days
