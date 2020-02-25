from cobble.target import *
import pytest

def package_fixture():
    return None

def test_target_creation_checks():
    def mktgt(**kws):
        Target(
            package = package_fixture(),
            name = "name",
            **kws
        )

    with pytest.raises(Exception): mktgt(using_and_products = 2)
    with pytest.raises(Exception): mktgt(using_and_products = "hi")
    with pytest.raises(Exception): mktgt(using_and_products = None)

    mktgt(using_and_products = lambda tgt, env: (None, []))
