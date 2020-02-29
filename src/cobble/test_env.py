"""Test suite for cobble.env

This is a pytest suite. You can run it by installing pytest and running pytest
from the root of the repository.
"""

import cobble
from cobble.env import *
import pytest

# Wrapper to apply is_frozen every time
def freeze(x):
    result = cobble.env.freeze(x)
    assert is_frozen(result), \
            "any result of freeze should be frozen, but got: %r" % result
    return result

def test_freeze():
    assert freeze("hi") == "hi", "freeze(str) is identity"
    assert freeze(['hi', 'there']) == ('hi', 'there'), "lists become tuples"
    assert freeze([['a', 'b'], 'c']) == (('a', 'b'), 'c'), "nested lists work"
    assert freeze(True) is True
    assert freeze(False) is False
    assert freeze(set(['a', 'b'])) == frozenset(['a', 'b'])
    assert freeze(frozenset(['a', 'b'])) == frozenset(['a', 'b'])
    assert freeze(None) is None

    # Numbers are not currently supported
    with pytest.raises(TypeError):
        freeze(3)

    # No dicts in the environment for now
    with pytest.raises(TypeError):
        freeze({})

def test_registry():
    r = KeyRegistry()

    assert ('foo' in r) == False, "must not initially contain key"

    r.define(EnvKey('foo',
        from_literal = freeze,
        default = 'bar'))

    assert 'foo' in r
    assert set(r) == set(['foo'])
    assert isinstance(r['foo'], EnvKey)
    assert len(r) == 1

    with pytest.raises(TypeError, match='deletion'):
        del r['foo']

    with pytest.raises(Exception, match='twice'):
        r.define(EnvKey('foo'))

    with pytest.raises(Exception, match='EnvKey'):
        r.define('foo')


def test_env():
    r = KeyRegistry()

    r.define(EnvKey('k1',
        from_literal = freeze,
        default = 'hello'))

    r.define(EnvKey('k2',
        from_literal = freeze,
        combine = lambda a, b: a + b,
        default = []))

    e = Env(r, {})
    assert len(e) == 0
    assert e['k1'] == 'hello'
    assert e['k2'] == ()
    with pytest.raises(KeyError): e['k3']

    e = e.derive({'k1': ['a']})
    assert e['k1'] == ('a',)
    assert e['k2'] == ()

    with pytest.raises(Exception, match='unique value'):
        e.derive({'k1': ['b']})

    e = e.derive({'k2': ['x']})
    assert e['k2'] == ('x',)
    e = e.derive({'k2': ['y']})
    assert e['k2'] == ('x','y')

    with pytest.raises(TypeError, match='deletion'):
        del e['k1']

def test_prepare_delta():
    assert prepare_delta(freeze) is freeze, "functions just get returned"
    assert prepare_delta(None) is None, "None just gets returned"

    assert prepare_delta({'x': ['1', '2']}) == {'x': ('1', '2')}, \
            "dict values get frozen"

def test_rewrite():
    r = KeyRegistry()

    # k1 will be a string-list key that joins with spaces. This is roughly how
    # c_flags works.
    r.define(EnvKey('k1',
        from_literal = freeze,
        # combine is concatenate.
        combine = lambda a, b: a + b,
        # readout is join
        readout = lambda elts: " ".join(elts),
        default = (),
    ))

    # k2 will be a single-value key.
    r.define(EnvKey('k2',
        from_literal = freeze,
        default = 'default',
    ))

    e = Env(r, {
        'k1': ['1', '2'],
    })

    assert e.rewrite('hello, world') == 'hello, world'
    assert e.rewrite('hello, $k2') == 'hello, default'
    assert e.rewrite('hello, $k1') == 'hello, 1 2'

    # Change k2 away from the default:
    e2 = e.derive({'k2': 'overridden'})
    assert e2.rewrite('hello, $k2') == 'hello, overridden'

    # Access a bogus key:
    with pytest.raises(KeyError): e.rewrite('hello, $unknown')

    # Use a delta that requires rewrite itself:
    e2 = e.derive({'k2': '<$k1>'})
    assert e2['k2'] == '<1 2>'
    assert e2.rewrite('hello, $k2') == 'hello, <1 2>'

def test_digest():
    r = KeyRegistry()

    # k1 will be a string-list key that joins with spaces. This is roughly how
    # c_flags works.
    r.define(EnvKey('k1',
        from_literal = freeze,
        # combine is concatenate.
        combine = lambda a, b: a + b,
        # readout is join
        readout = lambda elts: " ".join(elts),
        default = (),
    ))

    # k2 will be a single-value key.
    r.define(EnvKey('k2',
        from_literal = freeze,
        # Override combine to allow simple deletion.
        combine = lambda old, new: None if new is None else new,
        default = 'default',
    ))
    e = Env(r, {})
    empty_digest = e.digest

    e = e.derive({
        'k2': 'hi',
    })
    assert e.digest != empty_digest

    e = e.derive({'k2': None})
    assert e.digest == empty_digest

    


