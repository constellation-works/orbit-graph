import mod


def test_process_dynamic():
    fn = getattr(mod, "process")
    assert fn(5) == 6
