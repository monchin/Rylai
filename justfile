set windows-shell := ["powershell", "-NoLogo", "-Command"]

gen-pyi example:
    cargo run -- ./examples/{{example}} -o ./examples/{{example}}/python/{{example}}

gen-pyi-examples:
    @just gen-pyi add_content_sample
    @just gen-pyi basic_function_sample
    @just gen-pyi cross_module_sample
    @just gen-pyi macro_expand_sample
    @just gen-pyi override_sample
