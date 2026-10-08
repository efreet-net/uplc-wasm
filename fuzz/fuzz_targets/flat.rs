#![no_main]
use libfuzzer_sys::fuzz_target;
use uplc_conformance::{Budget, Mode, Profile, Program, Request};

fuzz_target!(|bytes: &[u8]| {
    let mut profile: serde_json::Value =
        serde_json::from_str(include_str!("../../profiles/plutus-v3-pv11.json")).unwrap();
    profile.as_object_mut().unwrap().remove("provenance");
    let request = Request {
        schema_version: 1,
        id: "fuzz/flat".into(),
        profile: serde_json::from_value::<Profile>(profile).unwrap(),
        program: Program::Flat {
            hex: hex::encode(bytes),
        },
        mode: Mode::Restricting {
            budget: Budget::new(1_000_000, 1_000_000),
        },
    };
    let _ = uplc_core::evaluate(&request);
});
