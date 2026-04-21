from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .macro_expand_sample import add, Foo, Bar, Baz, Qux
else:
    from .macro_expand_sample import *
