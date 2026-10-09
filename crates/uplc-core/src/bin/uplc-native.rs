fn main() -> std::io::Result<()> {
    uplc_conformance::serve("uplc-core", uplc_core::BUILD_REVISION, uplc_core::evaluate)
}
