"""Synthetic navigation fixture; direct module-local calls only."""


def normalize(value):
    return abs(value)


def price(value):
    return normalize(value) * 3


def invoice(value):
    return price(value) + 1


def preview(value):
    return normalize(value)


def price_preview(value):
    return value + 100


def dispatch(callback, value):
    return callback(value)  # callback identity is supplied at runtime


def test_invoice():
    assert invoice(-2) == 7


def test_preview():
    assert preview(-2) == 2
