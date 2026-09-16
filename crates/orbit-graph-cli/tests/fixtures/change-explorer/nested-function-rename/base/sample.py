def outer():
    def snapshot():
        return 1

    return snapshot()
