set windows-shell := ["cmd.exe", "/c"]
set windows-powershell := true

gen-pyi example:
    cargo run -- ./examples/{{example}} -o ./examples/{{example}}/python/{{example}}

examples-gen-pyi:
    @just gen-pyi basic_function_sample
    @just gen-pyi cross_module_sample
    @just gen-pyi macro_expand_sample
    @just gen-pyi override_sample
