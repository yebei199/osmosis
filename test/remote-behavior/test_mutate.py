from mutate import third_party_compiles


def test_third_party_compiles_skips_workspace_paths():
    log = """   Compiling ui v0.1.0 (/tmp/fault-x/snapshot/crates/ui)
   Compiling proc-macro2 v1.0.95
   Compiling slint v1.13.0 (https://github.com/yebei199/slint?branch=dev#abc)
    Finished `dev` profile
"""
    assert third_party_compiles(log) == [
        "Compiling proc-macro2 v1.0.95",
        "Compiling slint v1.13.0 (https://github.com/yebei199/slint?branch=dev#abc)",
    ]
