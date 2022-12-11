//! A test WASM module that checkes that the `time` module in the std lib works correctly.

use wasmer::TypedFunction;

use crate::{Runtime, SecretsStore};

#[test]
fn now() -> Result<(), Box<dyn std::error::Error>> {
    let (store, key) = super::setup_test_contract("test_contract_2")?;
    let mut runtime = Runtime::build(store, SecretsStore::default(), false).unwrap();
    runtime.enable_wasi = true;

    let module = runtime.prepare_contract_call(&key, &vec![].into(), 1_000)?;
    let f: TypedFunction<(), ()> = module
        .exports
        .get_function("time_func")?
        .typed(&runtime.wasm_store)?;
    f.call(&mut runtime.wasm_store)?;
    Ok(())
}
