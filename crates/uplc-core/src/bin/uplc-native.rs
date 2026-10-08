fn main() -> std::io::Result<()> {
    uplc_conformance::serve("uplc-core", env!("CARGO_PKG_VERSION"), uplc_core::evaluate)
}
