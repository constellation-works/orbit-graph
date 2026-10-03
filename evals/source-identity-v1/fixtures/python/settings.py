"""Synthetic source; this module must never be imported by the scorer."""
RETRY_LIMIT = 7
WINDOW: int = 12
LABELS = ("north", "south")
DYNAMIC = compute_limit()
ALIAS = RETRY_LIMIT

def decode(value):
    return value

class Packet:
    def decode(self, value):
        return value

# def counterfeit(): pass
TEXT = "def counterfeit(): pass"
