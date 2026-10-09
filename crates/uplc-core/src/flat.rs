//! Independent raw Flat decoding for primitive UPLC at PlutusV3 / protocol 11.
//!
//! The format rules are the pinned Plutus specification, revision
//! `5f785edeac0d1d89622d44344fdda07ef48e8c73`,
//! `doc/plutus-core-spec/flat-serialisation.tex`: in particular Padding,
//! Natural numbers, Programs, Built-in types, and Variable names. The pinned
//! `UntypedPlutusCore/Core/Instance/Flat.hs` supplies the term version gates;
//! `PlutusCore/FlatInstances.hs` specifies Word64 De Bruijn indices. No oracle
//! implementation is linked into this decoder.
//!
//! Supported programs consume their entire encoding, including exact `0*1`
//! alignment to the next byte (an aligned position still needs `00000001`).
//! Nonminimal natural-number encodings and noncanonical bytestring chunk sizes
//! are valid. Zero variable indices are rejected by the specification's `n>0`
//! rule, independently of the permissive raw reference APIs.
//!
//! Outside this milestone, decoding stops with `Unsupported`: unimplemented
//! builtin tags and complete constant type headers are checked first, but their payloads, later
//! subterms, and final padding are not fully validated. Constr/case version gates
//! are checked, and constr tags and the first fields-list bit are read. This is
//! deliberately not a claim of malformed-input coverage for unsupported syntax.
//! Resource limits also return `Unsupported`, never semantic/decode failure.

use num_bigint::{BigInt, BigUint};

use crate::{
    ast::{Constant, Program, Term},
    builtin::Builtin,
    error::DecodeError,
    limits::{MAX_AST_DEPTH, MAX_AST_NODES, MAX_CONSTANT_BYTES, MAX_FLAT_BYTES, MAX_INTEGER_BYTES},
};

/// Decode a raw, unwrapped Flat program under the initial V3/PV11 profile.
/// This function checks syntax; positive unbound variables are runtime errors.
pub fn decode(bytes: &[u8]) -> Result<Program, DecodeError> {
    if bytes.len() > MAX_FLAT_BYTES {
        return Err(DecodeError::Unsupported(format!(
            "raw Flat input exceeds {MAX_FLAT_BYTES} bytes"
        )));
    }
    let mut reader = Reader { bytes, bit: 0 };
    let version = [reader.word64()?, reader.word64()?, reader.word64()?];
    if !matches!(version, [1, 0, 0] | [1, 1, 0]) {
        return Err(malformed(format!(
            "UPLC version {}.{}.{} is not available in PlutusV3 / protocol 11",
            version[0], version[1], version[2]
        )));
    }

    // Each pending node already has a location, so parsing and destruction are
    // iterative. Apply children are visited function first, in wire order.
    let mut terms = vec![Term::Error];
    let mut pending = vec![(0, 0)];
    while let Some((id, depth)) = pending.pop() {
        if depth > MAX_AST_DEPTH {
            return Err(DecodeError::Unsupported(format!(
                "AST depth exceeds {MAX_AST_DEPTH}"
            )));
        }
        terms[id] = match reader.bits(4)? {
            0 => {
                let index = reader.word64()?;
                if index == 0 {
                    return Err(malformed("De Bruijn variable index must be positive"));
                }
                Term::Var(index)
            }
            tag @ (1 | 2 | 5) => {
                let child = allocate(&mut terms, 1)?;
                pending.push((child, depth + 1));
                match tag {
                    1 => Term::Delay(child),
                    2 => Term::Lambda(child),
                    _ => Term::Force(child),
                }
            }
            3 => {
                let function = allocate(&mut terms, 2)?;
                let argument = function + 1;
                pending.push((argument, depth + 1));
                pending.push((function, depth + 1));
                Term::Apply { function, argument }
            }
            4 => Term::Constant(reader.constant()?),
            6 => Term::Error,
            7 => {
                let tag = reader.bits(7)?;
                // The pinned spec's six builtin batches have contiguous tags
                // 0..=100, all available in this profile.
                if tag > 100 {
                    return Err(malformed(format!("unknown builtin tag {tag}")));
                }
                let builtin = Builtin::from_tag(tag).ok_or_else(|| {
                    DecodeError::Unsupported(format!(
                        "builtin {tag} is outside the implemented evaluator subset"
                    ))
                })?;
                Term::Builtin(builtin)
            }
            tag @ (8 | 9) => {
                if version == [1, 0, 0] {
                    return Err(malformed("constr/case require UPLC version 1.1.0"));
                }
                if tag == 8 {
                    reader.word64()?;
                    reader.bits(1)?;
                }
                return Err(DecodeError::Unsupported(
                    "constr/case are outside the implemented evaluator subset".into(),
                ));
            }
            tag => return Err(malformed(format!("unknown term tag {tag}"))),
        };
    }
    reader.padding()?;
    if reader.bit != bytes.len() * 8 {
        return Err(malformed(
            "trailing bytes after the program's final padding",
        ));
    }
    Program::new(version, terms, 0)
}

fn allocate(terms: &mut Vec<Term>, count: usize) -> Result<usize, DecodeError> {
    if terms.len() + count > MAX_AST_NODES {
        return Err(DecodeError::Unsupported(format!(
            "AST exceeds {MAX_AST_NODES} nodes"
        )));
    }
    let first = terms.len();
    terms.resize(first + count, Term::Error);
    Ok(first)
}

fn malformed(reason: impl Into<String>) -> DecodeError {
    DecodeError::Malformed(reason.into())
}

struct Reader<'a> {
    bytes: &'a [u8],
    bit: usize,
}

impl Reader<'_> {
    fn bits(&mut self, width: usize) -> Result<u8, DecodeError> {
        debug_assert!(width <= 8);
        if width > self.bytes.len() * 8 - self.bit {
            return Err(malformed("unexpected end of input"));
        }
        let mut value = 0;
        for _ in 0..width {
            value = (value << 1) | ((self.bytes[self.bit / 8] >> (7 - self.bit % 8)) & 1);
            self.bit += 1;
        }
        Ok(value)
    }

    /// Flat naturals use little-endian seven-bit groups, with continuation in
    /// each group's top bit. Extra zero groups are legal, including beyond ten
    /// groups; only the decoded Word64 value, not encoded length, is bounded.
    fn word64(&mut self) -> Result<u64, DecodeError> {
        let mut result = 0_u64;
        let mut shift = 0;
        loop {
            let group = self.bits(8)?;
            let payload = u64::from(group & 0x7f);
            if payload != 0 {
                if shift >= 64 || payload > u64::MAX >> shift {
                    return Err(malformed("natural number exceeds Word64"));
                }
                result |= payload << shift;
            }
            if group & 0x80 == 0 {
                return Ok(result);
            }
            shift = (shift + 7).min(64);
        }
    }

    fn integer(&mut self) -> Result<BigInt, DecodeError> {
        // Pack into little-endian bytes in one linear pass, then construct one
        // BigInt. Do not repeatedly shift a growing BigInt. Redundant zero
        // groups use no extra allocation, even for a maximally sized input.
        let mut packed = Vec::new();
        let mut shift = 0;
        loop {
            let group = self.bits(8)?;
            let payload = group & 0x7f;
            if payload != 0 {
                let significant_bits = shift + (8 - payload.leading_zeros() as usize);
                // Zigzag needs at most one extra bit beyond the magnitude.
                if significant_bits > MAX_INTEGER_BYTES * 8 + 1 {
                    return Err(DecodeError::Unsupported(format!(
                        "integer magnitude exceeds {MAX_INTEGER_BYTES} bytes"
                    )));
                }
                packed.resize(packed.len().max(significant_bits.div_ceil(8)), 0);
                let word = u16::from(payload) << (shift % 8);
                packed[shift / 8] |= word as u8;
                if word > 255 {
                    packed[shift / 8 + 1] |= (word >> 8) as u8;
                }
            }
            if group & 0x80 == 0 {
                break;
            }
            // The raw input limit bounds shift to less than 8 * MAX_FLAT_BYTES.
            shift += 7;
        }
        let negative = packed.first().is_some_and(|byte| byte & 1 != 0);
        let mut value = BigInt::from(BigUint::from_bytes_le(&packed) >> 1_usize);
        if negative {
            value = -value - 1;
        }
        if value.bits() > (MAX_INTEGER_BYTES * 8) as u64 {
            return Err(DecodeError::Unsupported(format!(
                "integer magnitude exceeds {MAX_INTEGER_BYTES} bytes"
            )));
        }
        Ok(value)
    }

    fn padding(&mut self) -> Result<(), DecodeError> {
        let width = 8 - self.bit % 8;
        if self.bits(width)? != 1 {
            return Err(malformed(
                "padding must be zeros followed by one at the byte boundary",
            ));
        }
        Ok(())
    }

    fn bytestring(&mut self) -> Result<Vec<u8>, DecodeError> {
        self.padding()?;
        let mut value = Vec::new();
        loop {
            let count = usize::from(self.bits(8)?);
            if count == 0 {
                return Ok(value);
            }
            if value.len() + count > MAX_CONSTANT_BYTES {
                return Err(DecodeError::Unsupported(format!(
                    "constant exceeds {MAX_CONSTANT_BYTES} bytes"
                )));
            }
            // padding and byte-sized chunks keep this cursor aligned.
            let start = self.bit / 8;
            let end = start + count;
            let chunk = self
                .bytes
                .get(start..end)
                .ok_or_else(|| malformed("truncated bytestring chunk"))?;
            value.extend_from_slice(chunk);
            self.bit = end * 8;
        }
    }

    fn type_tag(&mut self) -> Result<Option<u8>, DecodeError> {
        if self.bits(1)? == 0 {
            Ok(None)
        } else {
            self.bits(4).map(Some)
        }
    }

    /// Validate a complete type header iteratively, including constructor
    /// arities. Return only primitive tags; unsupported value payloads are not
    /// inspected. The raw input bound also bounds this allocation-free scan.
    fn constant_type(&mut self) -> Result<u8, DecodeError> {
        let mut pending_types = 1;
        let mut root = None;
        while pending_types != 0 {
            let tag = self
                .type_tag()?
                .ok_or_else(|| malformed("incomplete constant type"))?;
            root.get_or_insert(tag);
            pending_types -= 1;
            match tag {
                0..=4 | 8 | 13 => {}
                7 => match self.type_tag()? {
                    Some(5 | 12) => pending_types += 1,
                    Some(7) => {
                        if self.type_tag()? != Some(6) {
                            return Err(malformed("invalid pair type application"));
                        }
                        pending_types += 2;
                    }
                    _ => return Err(malformed("invalid constant type application")),
                },
                9..=11 => {
                    return Err(malformed("BLS element constants have no Flat encoding"));
                }
                _ => return Err(malformed(format!("invalid constant type tag {tag}"))),
            }
        }
        if self.type_tag()?.is_some() {
            return Err(malformed("extra tags after the constant type"));
        }
        match root {
            Some(tag @ 0..=4) => Ok(tag),
            _ => Err(DecodeError::Unsupported(
                "complex constants are outside the implemented evaluator subset".into(),
            )),
        }
    }

    fn constant(&mut self) -> Result<Constant, DecodeError> {
        Ok(match self.constant_type()? {
            0 => Constant::Integer(self.integer()?),
            1 => Constant::ByteString(self.bytestring()?),
            2 => Constant::String(
                String::from_utf8(self.bytestring()?)
                    .map_err(|_| malformed("string constant is not valid UTF-8"))?,
            ),
            3 => Constant::Unit,
            4 => Constant::Bool(self.bits(1)? != 0),
            _ => unreachable!("constant_type returns only primitive types"),
        })
    }
}

#[cfg(test)]
mod tests {
    use num_bigint::Sign;

    use super::*;

    /// Small test encoder follows the specification, not a candidate result.
    /// Literal encodings below separately anchor tags, bit order, and padding.
    #[derive(Default)]
    struct Bits {
        bytes: Vec<u8>,
        len: usize,
    }

    impl Bits {
        fn program() -> Self {
            let mut bits = Self::default();
            for version in [1, 0, 0] {
                bits.write(version, 8);
            }
            bits
        }

        fn write(&mut self, value: u8, width: usize) {
            for offset in (0..width).rev() {
                if self.len.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                self.bytes[self.len / 8] |= ((value >> offset) & 1) << (7 - self.len % 8);
                self.len += 1;
            }
        }

        fn natural(&mut self, mut value: u64) {
            loop {
                let chunk = (value & 127) as u8;
                value >>= 7;
                self.write(chunk | if value == 0 { 0 } else { 128 }, 8);
                if value == 0 {
                    break;
                }
            }
        }

        fn constant_type(&mut self, tags: &[u8]) {
            self.write(4, 4);
            for tag in tags {
                self.write(1, 1);
                self.write(*tag, 4);
            }
            self.write(0, 1);
        }

        fn integer(&mut self, value: &BigInt) {
            self.constant_type(&[0]);
            let encoded: BigInt = if value.sign() == Sign::Minus {
                -value * 2 - 1
            } else {
                value * 2
            };
            let (_, digits) = encoded.to_radix_le(128);
            for (index, digit) in digits.iter().enumerate() {
                self.write(*digit | if index + 1 == digits.len() { 0 } else { 128 }, 8);
            }
        }

        fn padding(&mut self) {
            self.write(1, 8 - self.len % 8);
        }

        fn finish(mut self) -> Vec<u8> {
            self.padding();
            self.bytes
        }
    }

    fn from_hex(encoded: &str) -> Result<Program, DecodeError> {
        decode(&hex::decode(encoded).unwrap())
    }

    fn constant(encoded: &str) -> Constant {
        let program = from_hex(encoded).unwrap();
        let Term::Constant(value) = &program.terms[program.root] else {
            panic!("expected a constant")
        };
        value.clone()
    }

    fn malformed_bytes(bytes: &[u8]) {
        assert!(matches!(decode(bytes), Err(DecodeError::Malformed(_))));
    }

    fn unsupported_bytes(bytes: &[u8]) {
        assert!(matches!(decode(bytes), Err(DecodeError::Unsupported(_))));
    }

    #[test]
    fn literal_primitive_encodings_and_identity() {
        assert_eq!(constant("010000481501"), Constant::Integer(42.into()));
        assert_eq!(constant("010000480041"), Constant::Integer((-1).into()));
        assert_eq!(
            constant("010000488102aabb0001"),
            Constant::ByteString(vec![0xaa, 0xbb])
        );
        assert_eq!(constant("01000048810001"), Constant::ByteString(vec![]));
        assert_eq!(
            constant("010000490102c3a90001"),
            Constant::String("é".into())
        );
        assert_eq!(constant("0100004981"), Constant::Unit);
        assert_eq!(constant("0100004a01"), Constant::Bool(false));
        assert_eq!(constant("0100004a21"), Constant::Bool(true));
        let identity = from_hex("010000200101").unwrap();
        assert_eq!(identity.terms, vec![Term::Lambda(1), Term::Var(1)]);
        assert_eq!(from_hex("01000061").unwrap().terms, vec![Term::Error]);
    }

    #[test]
    fn term_children_follow_flat_source_order() {
        // Apply (Lambda (Var 1)) (Force (Delay (Constant unit))).
        let mut bits = Bits::program();
        for tag in [3, 2, 0] {
            bits.write(tag, 4);
        }
        bits.natural(1);
        for tag in [5, 1] {
            bits.write(tag, 4);
        }
        bits.constant_type(&[3]);
        let program = decode(&bits.finish()).unwrap();
        assert_eq!(
            program.terms,
            vec![
                Term::Apply {
                    function: 1,
                    argument: 2
                },
                Term::Lambda(3),
                Term::Force(4),
                Term::Var(1),
                Term::Delay(5),
                Term::Constant(Constant::Unit),
            ]
        );
    }

    #[test]
    fn supported_encodings_reject_every_truncated_prefix() {
        for encoded in [
            "010000200101",
            "010000481501",
            "010000488102aabb0001",
            "010000490102c3a90001",
            "0100004981",
            "0100004a21",
            "01000061",
        ] {
            let bytes = hex::decode(encoded).unwrap();
            for end in 0..bytes.len() {
                malformed_bytes(&bytes[..end]);
            }
        }
        // Missing a continuation group rather than just final program padding.
        malformed_bytes(&[0x80]);
    }

    #[test]
    fn padding_requires_the_exact_next_byte_boundary_and_no_trailing_bytes() {
        for offset in 0..8 {
            let valid = [1_u8];
            let mut reader = Reader {
                bytes: &valid,
                bit: offset,
            };
            reader.padding().unwrap();
            assert_eq!(reader.bit, 8);
            for wrong in [0, 2, 3, 0x80] {
                let invalid = [wrong];
                let mut reader = Reader {
                    bytes: &invalid,
                    bit: offset,
                };
                // Ignore bits preceding the cursor; check every altered filler.
                if wrong & ((1_u16 << (8 - offset)) - 1) as u8 != 1 {
                    assert!(reader.padding().is_err());
                }
            }
        }
        for encoded in [
            "01000060",
            "01000062",
            "0100006801",
            "0100006100",
            "0100006101",
            "010000488100",
        ] {
            malformed_bytes(&hex::decode(encoded).unwrap());
        }
        // An early 1 followed by zeros is not legal bytestring alignment.
        malformed_bytes(&hex::decode("01000048820001").unwrap());
    }

    #[test]
    fn versions_and_term_tags_are_gated_independently_of_raw_references() {
        assert!(from_hex("01010061").is_ok());
        for encoded in [
            "00000061", "01000161", "01020061", "02000061", "01000081", "01000091",
        ] {
            malformed_bytes(&hex::decode(encoded).unwrap());
        }
        for tag in 10..=15 {
            malformed_bytes(&[1, 0, 0, (tag << 4) | 1]);
        }
        for tag in [8, 9] {
            let mut bits = Bits::program();
            bits.bytes[1] = 1;
            bits.write(tag, 4);
            if tag == 8 {
                bits.natural(0);
                bits.write(0, 1);
            }
            unsupported_bytes(&bits.finish());
        }
    }

    #[test]
    fn word64_variable_bounds_and_specification_zero_rule() {
        for value in [1, 127, 128, u32::MAX as u64 + 1, u64::MAX] {
            let mut bits = Bits::program();
            bits.write(0, 4);
            bits.natural(value);
            assert_eq!(
                decode(&bits.finish()).unwrap().terms,
                vec![Term::Var(value)]
            );
        }
        let mut zero = Bits::program();
        zero.write(0, 4);
        zero.natural(0);
        malformed_bytes(&zero.finish());
        let mut overflow = Bits::program();
        overflow.write(0, 4);
        for _ in 0..9 {
            overflow.write(128, 8);
        }
        overflow.write(2, 8); // 2^64, not a host-sized wrapping index.
        malformed_bytes(&overflow.finish());
        let mut long = Bits::program();
        long.write(0, 4);
        long.write(129, 8);
        for _ in 0..20 {
            long.write(128, 8);
        }
        long.write(0, 8);
        assert_eq!(decode(&long.finish()).unwrap().terms, vec![Term::Var(1)]);
    }

    #[test]
    fn nonminimal_version_and_integer_varints_are_valid() {
        let mut bits = Bits::default();
        for byte in [129, 128, 0, 128, 0, 0] {
            bits.write(byte, 8);
        }
        bits.constant_type(&[0]);
        for byte in [212, 128, 0] {
            bits.write(byte, 8);
        }
        assert_eq!(
            decode(&bits.finish()).unwrap().terms,
            vec![Term::Constant(Constant::Integer(42.into()))]
        );
    }

    #[test]
    fn integers_are_arbitrary_precision_with_signed_zigzag() {
        for text in [
            "0",
            "1",
            "-1",
            "127",
            "-128",
            "9007199254740993",
            "-9007199254740993",
            "18446744073709551616",
            "-340282366920938463463374607431768211457",
        ] {
            let value = text.parse::<BigInt>().unwrap();
            let mut bits = Bits::program();
            bits.integer(&value);
            assert_eq!(
                decode(&bits.finish()).unwrap().terms,
                vec![Term::Constant(Constant::Integer(value))]
            );
        }
    }

    #[test]
    fn integer_bound_is_on_magnitude_not_zigzag_or_redundant_groups() {
        let outside = BigInt::from(1) << (MAX_INTEGER_BYTES * 8);
        let largest: BigInt = &outside - 1;
        for value in [largest.clone(), -largest] {
            let mut bits = Bits::program();
            bits.integer(&value);
            assert_eq!(
                decode(&bits.finish()).unwrap().terms,
                vec![Term::Constant(Constant::Integer(value))]
            );
        }
        for value in [outside.clone(), -outside] {
            let mut bits = Bits::program();
            bits.integer(&value);
            unsupported_bytes(&bits.finish());
        }
        let mut redundant = Bits::program();
        redundant.constant_type(&[0]);
        for _ in 0..MAX_INTEGER_BYTES + 1 {
            redundant.write(128, 8);
        }
        redundant.write(0, 8);
        assert_eq!(
            decode(&redundant.finish()).unwrap().terms,
            vec![Term::Constant(Constant::Integer(0.into()))]
        );
    }

    #[test]
    fn maximally_sized_redundant_integer_uses_bounded_linear_scan() {
        let mut bits = Bits::program();
        bits.constant_type(&[0]);
        // At this offset each integer group occupies one more encoded byte;
        // the version, type header, and final padding take five bytes total.
        for _ in 0..MAX_FLAT_BYTES - 6 {
            bits.write(128, 8);
        }
        bits.write(0, 8);
        let encoded = bits.finish();
        assert_eq!(encoded.len(), MAX_FLAT_BYTES);
        assert_eq!(
            decode(&encoded).unwrap().terms,
            vec![Term::Constant(Constant::Integer(0.into()))]
        );
    }

    #[test]
    fn byte_chunks_need_termination_and_strings_need_valid_utf8() {
        // The specification permits chunks shorter than 255 before the last.
        assert_eq!(
            constant("010000488101aa01bb0001"),
            Constant::ByteString(vec![0xaa, 0xbb])
        );
        for encoded in [
            "010000488102aa",
            "010000488101aa",
            "010000490102c0800001",
            "010000490101ff0001",
            "010000490103eda0800001",
        ] {
            malformed_bytes(&hex::decode(encoded).unwrap());
        }
    }

    #[test]
    fn constant_size_limit_is_explicitly_unsupported() {
        for size in [MAX_CONSTANT_BYTES, MAX_CONSTANT_BYTES + 1] {
            let mut bits = Bits::program();
            bits.constant_type(&[1]);
            bits.padding();
            let bytes = vec![0xab; size];
            for chunk in bytes.chunks(255) {
                bits.write(chunk.len() as u8, 8);
                for byte in chunk {
                    bits.write(*byte, 8);
                }
            }
            bits.write(0, 8);
            let encoded = bits.finish();
            if size == MAX_CONSTANT_BYTES {
                assert_eq!(
                    decode(&encoded).unwrap().terms,
                    vec![Term::Constant(Constant::ByteString(bytes))]
                );
            } else {
                unsupported_bytes(&encoded);
            }
        }
    }

    #[test]
    fn unimplemented_builtins_are_unsupported_even_under_lambda_and_delay() {
        for tag in [10, 54, 87, 100] {
            for prefix in [None, Some(1), Some(2)] {
                let mut bits = Bits::program();
                if let Some(prefix) = prefix {
                    bits.write(prefix, 4);
                }
                bits.write(7, 4);
                bits.write(tag, 7);
                unsupported_bytes(&bits.finish());
            }
        }
        for tag in [101, 127] {
            let mut bits = Bits::program();
            bits.write(7, 4);
            bits.write(tag, 7);
            malformed_bytes(&bits.finish());
        }
        malformed_bytes(&[1, 0, 0, 0x70]);
    }

    #[test]
    fn supported_builtins_are_structural_syntax_with_strict_complete_encodings() {
        for tag in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 26] {
            for prefix in [None, Some(1), Some(2), Some(5)] {
                let mut bits = Bits::program();
                if let Some(prefix) = prefix {
                    bits.write(prefix, 4);
                }
                bits.write(7, 4);
                bits.write(tag, 7);
                let encoded = bits.finish();
                let program = decode(&encoded).unwrap();
                assert_eq!(
                    program.terms.last(),
                    Some(&Term::Builtin(Builtin::from_tag(tag).unwrap()))
                );
                let expected = serde_json::json!(["builtin", tag.to_string()]);
                assert_eq!(
                    program.normalize().unwrap(),
                    match prefix {
                        None => expected,
                        Some(1) => serde_json::json!(["delay", expected]),
                        Some(2) => serde_json::json!(["lambda", expected]),
                        Some(5) => serde_json::json!(["force", expected]),
                        _ => unreachable!(),
                    }
                );
                for end in 0..encoded.len() {
                    malformed_bytes(&encoded[..end]);
                }
                let mut trailing = encoded.clone();
                trailing.push(0);
                malformed_bytes(&trailing);
                let mut invalid_filler = encoded;
                *invalid_filler.last_mut().unwrap() &= !1;
                malformed_bytes(&invalid_filler);
            }
        }
    }

    #[test]
    fn complex_type_headers_are_validated_before_unsupported() {
        for tags in [
            &[7, 5, 0][..],
            &[7, 7, 6, 0, 1],
            &[7, 12, 0],
            &[8],
            &[13],
            &[7, 5, 7, 7, 6, 4, 8],
        ] {
            let mut bits = Bits::program();
            bits.constant_type(tags);
            unsupported_bytes(&bits.finish());
        }
        for tags in [
            &[][..],
            &[0, 0],
            &[5],
            &[6],
            &[7],
            &[7, 5],
            &[7, 0, 0],
            &[7, 7, 5, 0, 0],
            &[7, 7, 6, 0],
            &[7, 5, 0, 0],
            &[9],
            &[10],
            &[11],
            &[12],
            &[14],
            &[15],
        ] {
            let mut bits = Bits::program();
            bits.constant_type(tags);
            malformed_bytes(&bits.finish());
        }
        // A type-list start without its four-bit tag.
        malformed_bytes(&[1, 0, 0, 0x48]);
    }

    #[test]
    fn depth_and_raw_input_limits_do_not_become_decode_failures() {
        for depth in [MAX_AST_DEPTH, MAX_AST_DEPTH + 1] {
            let mut bits = Bits::program();
            for _ in 0..depth {
                bits.write(2, 4);
            }
            bits.write(6, 4);
            let encoded = bits.finish();
            if depth == MAX_AST_DEPTH {
                assert_eq!(decode(&encoded).unwrap().terms.len(), depth + 1);
            } else {
                unsupported_bytes(&encoded);
            }
        }
        unsupported_bytes(&vec![0; MAX_FLAT_BYTES + 1]);
    }

    #[test]
    fn node_limit_is_checked_without_recursive_parsing() {
        for leaves in [MAX_AST_NODES / 2, MAX_AST_NODES / 2 + 1] {
            let mut bits = Bits::program();
            let mut pending = vec![leaves];
            while let Some(count) = pending.pop() {
                if count == 1 {
                    bits.write(6, 4);
                } else {
                    bits.write(3, 4);
                    pending.push(count / 2);
                    pending.push(count - count / 2);
                }
            }
            let encoded = bits.finish();
            if leaves * 2 - 1 <= MAX_AST_NODES {
                assert_eq!(decode(&encoded).unwrap().terms.len(), leaves * 2 - 1);
            } else {
                unsupported_bytes(&encoded);
            }
        }
    }
}
