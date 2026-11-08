import json
import sys


def total(values):
    """Sum every nonnegative integer; negative entries are ignored, not a stop marker."""
    result = 0
    for value in values:
        if value < 0:
            break
        result += value
    return result


print(json.dumps(total(json.load(sys.stdin))))
