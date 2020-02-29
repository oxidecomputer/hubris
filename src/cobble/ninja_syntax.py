"""Python module for generating .ninja files, derived from code provided by
the Ninja authors.

I've tightened semantics in some areas, and applied updates for Python 3.
"""

import collections
import itertools
import textwrap
import re

def _escape_path(word):
    """Used to escape paths; only escapes the characters that are significant
    in a build/rule definition.  Interestingly, does *not* escape dollar signs.
    """
    return str(word).replace('$ ','$$ ').replace(' ','$ ').replace(':', '$:')


def _as_iterable(input):
    """Allows punning of singleton values as iterables. Iterables are passed
    through, except strings, which are treated as values.   Other values emerge
    as singleton iterables, except None, which emerges as empty.
    """
    if isinstance(input, str):
        return [input]
    if isinstance(input, collections.Iterable):
        return input
    if input is None:
        return []
    return [input]


def _count_dollars_before_index(s, i):
    """Returns the number of '$' characters right in front of s[i]."""
    dollar_count = 0
    dollar_index = i - 1
    while dollar_index > 0 and s[dollar_index] == '$':
        dollar_count += 1
        dollar_index -= 1
    return dollar_count


class Writer(object):
    def __init__(self, output, width=78):
        self.output = output
        self.width = width

        self._indent = 0

    def newline(self):
        """Emits a blank line."""
        self.output.write('\n')

    def comment(self, text, wrap = True):
        """Emits some commented text."""
        if wrap:
            for line in textwrap.wrap(text, self.width - 2):
                self.output.write('# ' + line + '\n')
        else:
            self.output.write('# ' + text + '\n')

    def variable(self, key, value):
        """Emits a variable, joining values with spaces if required."""
        # TODO(cbiffle): neither key nor value are escaped?
        value_str = ' '.join(filter(None, _as_iterable(value)))

        if value_str:
          self._line('%s = %s' % (key, value_str))

    def pool(self, name, depth):
        """Emits a pool declaration."""
        # TODO(cbiffle): name is not escaped?
        self._line('pool %s' % name)
        with self._increase_indent():
          self.variable('depth', depth)

    def rule(self, name, command, description=None, depfile=None,
             generator=False, pool=None, restat=False, rspfile=None,
             rspfile_content=None, deps=None):
        """Emits a rule."""
        # TODO(cbiffle): name is not escaped?
        self._line('rule %s' % name)
        with self._increase_indent():
            self.variable('command', command)
            self.variable('description', description)
            self.variable('depfile', depfile)
            self.variable('pool', pool)
            self.variable('rspfile', rspfile)
            self.variable('rspfile_content', rspfile_content)
            self.variable('deps', deps)
            self.variable('generator', generator and '1')
            self.variable('restat', restat and '1')

    def build(self, outputs, rule, inputs=None, implicit=None, order_only=None,
              variables=None):
        """Emits a build product.

        Outputs, inputs, implicit, and order_only are typically iterables, but
        each can also be provided as a single string.

        Variables can either be a mapping or an iterable of key,value pairs.
        """
        # TODO(cbiffle): rule name not escaped?
        out_outputs = map(_escape_path, _as_iterable(outputs))
        all_inputs = map(_escape_path, _as_iterable(inputs))

        if implicit:
            all_inputs = itertools.chain(
                all_inputs,
                ['|'],
                map(_escape_path, _as_iterable(implicit)))
        if order_only:
            all_inputs = itertools.chain(
                all_inputs,
                ['||'],
                map(_escape_path, _as_iterable(order_only)))

        self._line('build %s: %s %s' % (' '.join(out_outputs),
                                        rule,
                                        ' '.join(all_inputs)))

        with self._increase_indent():
            if variables is None:
              pass
            elif isinstance(variables, collections.Mapping):
                for key in variables:
                    self.variable(key, variables[key])
            else:
                for key, val in variables:
                    self.variable(key, val)

    def include(self, path):
        """Emits an include statement for a path."""
        # TODO(cbiffle): path is not escaped ...?
        self._line('include %s' % path)

    def subninja(self, path):
        """Emits a subninja statement."""
        # TODO(cbiffle): path is not escaped ...?
        self._line('subninja %s' % path)

    def default(self, paths):
        """Designates some paths as default."""
        # TODO(cbiffle): paths not escaped?
        self._line('default %s' % ' '.join(_as_iterable(paths)))

    def _increase_indent(self):
        class Indenter:
            def __enter__(_self):
                self._indent += 1
            def __exit__(_self, *stuff):
                self._indent -= 1
        return Indenter()

    def _line(self, text):
        """Write 'text' word-wrapped at self.width characters."""
        leading_space = '  ' * self._indent
        while len(leading_space) + len(text) > self.width:
            # The text is too wide; wrap if possible.

            # Find the rightmost space that would obey our width constraint and
            # that's not an escaped space.
            available_space = self.width - len(leading_space) - len(' $')
            space = available_space
            while True:
              space = text.rfind(' ', 0, space)
              if space < 0 or \
                 _count_dollars_before_index(text, space) % 2 == 0:
                break

            if space < 0:
                # No such space; just use the first unescaped space we can find.
                space = available_space - 1
                while True:
                  space = text.find(' ', space + 1)
                  if space < 0 or \
                     _count_dollars_before_index(text, space) % 2 == 0:
                    break
            if space < 0:
                # Give up on breaking.
                break

            self.output.write(leading_space + text[0:space] + ' $\n')
            text = text[space+1:]

            # Subsequent lines are continuations, so indent them.
            leading_space = '  ' * (self._indent+2)

        self.output.write(leading_space + text + '\n')

    def close(self):
        self.output.close()


def escape(string):
    """Escape a string such that it can be embedded into a Ninja file without
    further interpretation."""
    assert '\n' not in string, 'Ninja syntax does not allow newlines'
    # We only have one special metacharacter: '$'.
    return string.replace('$', '$$')
