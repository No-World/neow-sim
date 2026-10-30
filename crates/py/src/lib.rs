//! neow-sim 的 Python 绑定：把统一 gym 协议（../docs/protocol.md）暴露给 neow-ai。
use pyo3::prelude::*;

#[pymodule]
fn neow_sim(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    // TODO: Env::new(seed, character, ascension) / reset() / step(action) / legal_actions()
    Ok(())
}
