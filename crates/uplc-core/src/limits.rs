//! Portable implementation bounds, not ledger validity or execution-cost rules.
//!
//! Crossing these bounds must produce `unsupported`. They bound native and Wasm
//! allocation/work even for large or zero-cost development models. Input and AST
//! traversal are iterative; the lower result-depth bound also bounds recursive
//! traversal inside the JSON serializer and JSON value destructor.

/// Raw Flat bytes (the JSON hex transport has its own separate size limit).
pub const MAX_FLAT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_AST_NODES: usize = 100_000;
/// Longest root-to-leaf path, with a leaf at depth zero.
pub const MAX_AST_DEPTH: usize = 512;
/// Bytes in a primitive bytestring or the UTF-8 encoding of a primitive string.
pub const MAX_CONSTANT_BYTES: usize = 1024 * 1024;
/// Bytes in the unsigned magnitude of a primitive integer, excluding its sign.
pub const MAX_INTEGER_BYTES: usize = 64 * 1024;
/// Expanded result nodes: sharing in an arena does not reduce this count.
pub const MAX_OUTPUT_NODES: usize = 100_000;
/// Structural JSON depth, with the result term at depth zero.
pub const MAX_OUTPUT_DEPTH: usize = 128;
/// Serialized normalized term size, leaving room in the 16 MiB response limit.
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Independent work bound, including under a supplied model with zero costs.
pub const MAX_MACHINE_STEPS: usize = 10_000_000;
/// Aggregate magnitude/string/bytes payload of constants produced at runtime.
/// Input constants remain borrowed; the arena never stores another input copy.
pub const MAX_RUNTIME_CONSTANT_BYTES: usize = 8 * 1024 * 1024;
