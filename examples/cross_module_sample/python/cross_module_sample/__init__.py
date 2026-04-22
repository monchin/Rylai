from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .aa import A
    from .bb import B

else:
    from .cross_module_sample import *
