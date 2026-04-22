from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .basic_function_sample import sum_as_string, add, C, SampleError

else:
    from .basic_function_sample import *
