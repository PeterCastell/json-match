//! Random test-JSON generation and the crate's test suite.
//!
//! `generate_test_json` materializes JSON that satisfies a chosen fraction of a
//! pattern's fields (`closeness`) surrounded by structured noise (`bloat`).
//! Predicates are ignored by the generator: a "satisfying" value has the right
//! path and type, so patterns fed to it should be predicate-free (or use
//! predicates that accept any generated value).

use crate::{FieldMatch, FieldType, PathSegment};
use rand::RngExt;
use rand::distr::Alphanumeric;
use rand::rngs::StdRng;

#[derive(Default)]
struct GenNode<'a> {
    key_children: Vec<(&'a str, usize)>,
    index_children: Vec<(u32, usize)>,
    any_index_child: Option<usize>,
    terminals: Vec<&'a FieldType>,
}

fn gen_child<'a>(nodes: &mut Vec<GenNode<'a>>, parent: usize, segment: &PathSegment<'a>) -> usize {
    fn push<'a>(nodes: &mut Vec<GenNode<'a>>) -> usize {
        nodes.push(GenNode::default());
        nodes.len() - 1
    }
    match segment {
        PathSegment::Key(key) => {
            if let Some(&(_, child)) = nodes[parent].key_children.iter().find(|(k, _)| k == key) {
                child
            } else {
                let child = push(nodes);
                nodes[parent].key_children.push((key, child));
                child
            }
        }
        PathSegment::Index(index) => {
            if let Some(&(_, child)) = nodes[parent]
                .index_children
                .iter()
                .find(|&&(i, _)| i == *index)
            {
                child
            } else {
                let child = push(nodes);
                nodes[parent].index_children.push((*index, child));
                child
            }
        }
        PathSegment::AnyIndex => match nodes[parent].any_index_child {
            Some(child) => child,
            None => {
                let child = push(nodes);
                nodes[parent].any_index_child = Some(child);
                child
            }
        },
    }
}

pub fn generate_test_json(
    fields: &[FieldMatch<'_>],
    closeness: f64,
    bloat: f64,
    rng: &mut StdRng,
) -> String {
    let bloat = if bloat.is_finite() {
        bloat.max(0.0)
    } else {
        0.0
    };
    let count = (closeness.clamp(0.0, 1.0) * fields.len() as f64).round() as usize;
    let mut indices: Vec<usize> = (0..fields.len()).collect();
    for i in 0..count {
        let j = rng.random_range(i..indices.len());
        indices.swap(i, j);
    }

    let mut nodes: Vec<GenNode<'_>> = vec![GenNode::default()];
    for &field_index in &indices[..count] {
        let field = &fields[field_index];
        let mut node = 0usize;
        for segment in field.path {
            node = gen_child(&mut nodes, node, segment);
        }
        nodes[node].terminals.push(&field.r#type);
    }

    let mut emitter = Emitter {
        nodes: &nodes,
        rng,
        bloat,
    };
    emitter.value(0, 1)
}

struct Emitter<'n, 'a, 'r> {
    nodes: &'n [GenNode<'a>],
    rng: &'r mut StdRng,
    bloat: f64,
}

impl Emitter<'_, '_, '_> {
    fn value(&mut self, node: usize, level: usize) -> String {
        // Structure demanded by children takes priority over terminal types:
        // a node used as a path prefix must be a container.
        if !self.nodes[node].key_children.is_empty() {
            self.object(node, level)
        } else if !self.nodes[node].index_children.is_empty()
            || self.nodes[node].any_index_child.is_some()
        {
            self.array(node, level)
        } else if let Some(ty) = self.nodes[node].terminals.first() {
            self.terminal(ty)
        } else {
            self.primitive()
        }
    }

    fn object(&mut self, node: usize, level: usize) -> String {
        let real_keys: Vec<&str> = self.nodes[node]
            .key_children
            .iter()
            .map(|&(k, _)| k)
            .collect();
        let mut members: Vec<String> = Vec::new();
        for i in 0..self.nodes[node].key_children.len() {
            let (name, child) = self.nodes[node].key_children[i];
            let value = self.value(child, level + 1);
            members.push(format!("{:?}:{}", name, value));
        }
        let spine_len = (level as f64 * self.bloat).floor() as usize;
        let extra = (self.bloat * real_keys.len().max(1) as f64 / 2.0).floor() as usize;
        let mut noise_values = Vec::new();
        if spine_len > 0 {
            noise_values.push(self.chain_value(spine_len));
        }
        while noise_values.len() < extra {
            let depth = self.rng.random_range(0..=2);
            noise_values.push(self.chain_value(depth));
        }
        for value in noise_values {
            let name = loop {
                let candidate = rand_name(self.rng);
                if !real_keys.iter().any(|&k| k == candidate) {
                    break candidate;
                }
            };
            let member = format!("{:?}:{}", name, value);
            let at = self.rng.random_range(0..=members.len());
            members.insert(at, member);
        }
        format!("{{{}}}", members.join(","))
    }

    fn array(&mut self, node: usize, level: usize) -> String {
        let base_len = self.nodes[node]
            .index_children
            .iter()
            .map(|&(i, _)| i as usize + 1)
            .max()
            .unwrap_or(0);
        let extra = usize::from(self.nodes[node].any_index_child.is_some())
            + (self.bloat * 2.0).floor() as usize;
        let mut slots: Vec<Option<usize>> = vec![None; base_len + extra];
        for i in 0..self.nodes[node].index_children.len() {
            let (index, child) = self.nodes[node].index_children[i];
            slots[index as usize] = Some(child);
        }
        if let Some(any_child) = self.nodes[node].any_index_child {
            // Never place the wildcard element at a fixed index: those slots
            // carry their own required structure.
            let free: Vec<usize> = slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| slot.is_none())
                .map(|(i, _)| i)
                .collect();
            let at = free[self.rng.random_range(0..free.len())];
            slots[at] = Some(any_child);
        }
        let elements: Vec<String> = slots
            .into_iter()
            .map(|slot| match slot {
                Some(child) => self.value(child, level + 1),
                None => self.primitive(),
            })
            .collect();
        format!("[{}]", elements.join(","))
    }

    fn terminal(&mut self, ty: &FieldType) -> String {
        match ty {
            FieldType::String => format!("{:?}", rand_name(self.rng)),
            FieldType::Number => noise_number(self.rng),
            FieldType::Bool => self.rng.random_bool(0.5).to_string(),
            FieldType::Null => "null".to_string(),
            FieldType::Object => {
                if self.bloat == 0.0 {
                    "{}".to_string()
                } else {
                    let len = self.rng.random_range(1..=2);
                    self.chain_object(len)
                }
            }
            FieldType::Array => {
                if self.bloat == 0.0 {
                    "[]".to_string()
                } else {
                    let len = self.rng.random_range(1..=2);
                    self.chain_array(len)
                }
            }
            FieldType::Literal(literal) => literal.to_string(),
            FieldType::Any => self.primitive(),
        }
    }

    fn chain_value(&mut self, len: usize) -> String {
        if len == 0 {
            self.primitive()
        } else if self.rng.random_bool(0.5) {
            self.chain_array(len)
        } else {
            self.chain_object(len)
        }
    }

    fn chain_array(&mut self, len: usize) -> String {
        let child = self.chain_value(len - 1);
        let mut items: Vec<String> = (0..self.sibling_count())
            .map(|_| self.primitive())
            .collect();
        let at = self.rng.random_range(0..=items.len());
        items.insert(at, child);
        format!("[{}]", items.join(","))
    }

    fn chain_object(&mut self, len: usize) -> String {
        let child = self.chain_value(len - 1);
        let mut members: Vec<String> = (0..self.sibling_count())
            .map(|_| format!("{:?}:{}", rand_name(self.rng), self.primitive()))
            .collect();
        let at = self.rng.random_range(0..=members.len());
        members.insert(at, format!("{:?}:{}", rand_name(self.rng), child));
        format!("{{{}}}", members.join(","))
    }

    fn sibling_count(&mut self) -> usize {
        let cap = ((self.bloat * 2.0).ceil() as usize).min(5);
        self.rng.random_range(0..=cap)
    }

    fn primitive(&mut self) -> String {
        match self.rng.random_range(0..4) {
            0 => format!("{:?}", rand_name(self.rng)),
            1 => noise_number(self.rng),
            2 => self.rng.random_bool(0.5).to_string(),
            _ => "null".to_string(),
        }
    }
}

fn rand_name(rng: &mut StdRng) -> String {
    let len = rng.random_range(3..=8);
    (0..len)
        .map(|_| char::from(rng.sample(Alphanumeric)))
        .collect()
}

fn noise_number(rng: &mut StdRng) -> String {
    if rng.random_bool(0.5) {
        rng.random_range(-1_000..1_000i64).to_string()
    } else {
        format!("{:.2}", rng.random_range(-1_000.0..1_000.0f64))
    }
}

/// A representative predicate-free pattern, shared by tests and benches.
pub fn test_fields() -> Vec<FieldMatch<'static>> {
    use PathSegment::*;
    vec![
        FieldMatch {
            path: &[Key("foo")],
            r#type: FieldType::String,
            predicate: None,
            capture: true,
        },
        FieldMatch {
            path: &[Key("a"), Key("b")],
            r#type: FieldType::Bool,
            predicate: None,
            capture: true,
        },
        FieldMatch {
            path: &[Key("a"), Key("c")],
            r#type: FieldType::Object,
            predicate: None,
            capture: true,
        },
        FieldMatch {
            path: &[Key("a"), Key("c"), Key("d")],
            r#type: FieldType::String,
            predicate: None,
            capture: true,
        },
        FieldMatch {
            path: &[Key("list"), Index(0), Key("name")],
            r#type: FieldType::String,
            predicate: None,
            capture: true,
        },
        FieldMatch {
            path: &[Key("list"), AnyIndex, Key("id")],
            r#type: FieldType::Number,
            predicate: None,
            capture: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::{generate_test_json, test_fields};
    use crate::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use regex::Regex;

    /// Machine capture indices keyed by (set index, field index, predicate group name).
    type CaptureIndices = std::collections::HashMap<(u32, u32, Option<String>), u32>;

    fn compile(sets: &[&[FieldMatch<'_>]]) -> (MatchMachine, CaptureIndices) {
        let mut captures = std::collections::HashMap::new();
        let machine = MatchMachine::compile(
            sets.iter().map(|fields| MatchSet {
                field_matches: fields,
            }),
            |args| {
                captures.insert(
                    (
                        args.match_set_index,
                        args.field_index,
                        args.predicate_capture_name.map(str::to_owned),
                    ),
                    args.capture_index_in_machine,
                );
            },
        )
        .unwrap();
        (machine, captures)
    }

    fn run(machine: &MatchMachine, input: &str) -> Result<MachineState, MatchError> {
        let mut state = machine.allocate_state();
        machine.match_string(input, &mut state)?;
        Ok(state)
    }

    fn field<'a>(
        path: &'a [PathSegment<'a>],
        r#type: FieldType,
        predicate: Option<&str>,
        capture: bool,
    ) -> FieldMatch<'a> {
        FieldMatch {
            path,
            r#type,
            predicate: predicate.map(|p| Regex::new(p).unwrap()),
            capture,
        }
    }

    use PathSegment::*;

    #[test]
    fn basic_nested_capture() {
        let fields = [field(&[Key("a"), Key("b")], FieldType::Bool, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, r#"{"x":1,"a":{"y":[3],"b":true},"z":null}"#).unwrap();
        assert!(state.result.did_match(0));
        assert_eq!(
            *state.result.capture(captures[&(0, 0, None)]),
            CaptureValue::Bool(true)
        );
    }

    #[test]
    fn type_mismatch_no_match() {
        let fields = [field(&[Key("a")], FieldType::Number, None, false)];
        let (machine, _) = compile(&[&fields]);
        let state = run(&machine, r#"{"a":"not a number"}"#).unwrap();
        assert!(!state.result.did_match(0));
    }

    #[test]
    fn all_capture_types() {
        let input = r#"{"o":{"k":1},"ar":[1,2],"s":"hi","n":-12.5e2,"b":false,"nul":null}"#;
        let fields = [
            field(&[Key("o")], FieldType::Object, None, true),
            field(&[Key("ar")], FieldType::Array, None, true),
            field(&[Key("s")], FieldType::String, None, true),
            field(&[Key("n")], FieldType::Number, None, true),
            field(&[Key("b")], FieldType::Bool, None, true),
            field(&[Key("nul")], FieldType::Null, None, true),
        ];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 0, None)]) {
            CaptureValue::Object(range) => {
                assert_eq!(&input[range_u32_to_usize(*range)], r#"{"k":1}"#)
            }
            other => panic!("expected Object, got {other:?}"),
        }
        match state.result.capture(captures[&(0, 1, None)]) {
            CaptureValue::Array(range) => assert_eq!(&input[range_u32_to_usize(*range)], "[1,2]"),
            other => panic!("expected Array, got {other:?}"),
        }
        match state.result.capture(captures[&(0, 2, None)]) {
            CaptureValue::String(s) => assert_eq!(s.resolve(input), "hi"),
            other => panic!("expected String, got {other:?}"),
        }
        assert_eq!(
            *state.result.capture(captures[&(0, 3, None)]),
            CaptureValue::Number(-1250.0)
        );
        assert_eq!(
            *state.result.capture(captures[&(0, 4, None)]),
            CaptureValue::Bool(false)
        );
        assert_eq!(
            *state.result.capture(captures[&(0, 5, None)]),
            CaptureValue::Null
        );
    }

    #[test]
    fn literal_and_any() {
        let input = r#"{"lit":[1,2,3],"any":{"deep":1}}"#;
        let fields = [
            field(
                &[Key("lit")],
                FieldType::Literal("[1,2,3]".into()),
                None,
                true,
            ),
            field(&[Key("any")], FieldType::Any, None, true),
        ];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 1, None)]) {
            CaptureValue::Object(range) => {
                assert_eq!(&input[range_u32_to_usize(*range)], r#"{"deep":1}"#)
            }
            other => panic!("expected Object, got {other:?}"),
        }
        // Literal is whitespace-sensitive.
        let state = run(&machine, r#"{"lit":[1, 2, 3],"any":0}"#).unwrap();
        assert!(!state.result.did_match(0));
    }

    #[test]
    fn borrowed_string_is_range_into_input() {
        let input = r#"{"s":"plain"}"#;
        let fields = [field(&[Key("s")], FieldType::String, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        match state.result.capture(captures[&(0, 0, None)]) {
            CaptureValue::String(UnescapedString::Borrowed(range)) => {
                assert_eq!(&input[range_u32_to_usize(*range)], "plain");
            }
            other => panic!("expected Borrowed, got {other:?}"),
        }
    }

    #[test]
    fn escaped_string_capture_owned() {
        let input = r#"{"s":"line1\nline2 A 😀 \\"}"#;
        let fields = [field(&[Key("s")], FieldType::String, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        match state.result.capture(captures[&(0, 0, None)]) {
            CaptureValue::String(s @ UnescapedString::Owned(_)) => {
                assert_eq!(s.resolve(input), "line1\nline2 A 😀 \\");
            }
            other => panic!("expected Owned, got {other:?}"),
        }
    }

    #[test]
    fn escaped_key_lookup() {
        let fields = [field(&[Key("a\nb")], FieldType::Number, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, r#"{"a\nb":7}"#).unwrap();
        assert!(state.result.did_match(0));
        assert_eq!(
            *state.result.capture(captures[&(0, 0, None)]),
            CaptureValue::Number(7.0)
        );
        // \u escape spelling of a key must also resolve.
        let fields = [field(&[Key("A")], FieldType::Number, None, false)];
        let (machine, _) = compile(&[&fields]);
        assert!(run(&machine, r#"{"A":1}"#).unwrap().result.did_match(0));
    }

    #[test]
    fn predicate_gates_match_and_captures_groups() {
        let input = r#"{"v":"hello-42"}"#;
        let fields = [field(
            &[Key("v")],
            FieldType::String,
            Some(r"^(?<word>\w+)-(?<_junk>\d+)$"),
            true,
        )];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        // '_'-prefixed groups get no capture slot.
        assert!(!captures.contains_key(&(0, 0, Some("_junk".to_owned()))));
        match state
            .result
            .capture(captures[&(0, 0, Some("word".to_owned()))])
        {
            CaptureValue::PredicateCapture(UnescapedString::Borrowed(range)) => {
                assert_eq!(&input[range_u32_to_usize(*range)], "hello");
            }
            other => panic!("expected Borrowed predicate capture, got {other:?}"),
        }
        // Predicate failure means the field is unsatisfied.
        let state = run(&machine, r#"{"v":"no dash here"}"#).unwrap();
        assert!(!state.result.did_match(0));
    }

    #[test]
    fn predicate_runs_on_unescaped_content() {
        // Raw JSON text is `x\ny`; the predicate must see a real newline.
        let input = r#"{"v":"x\ny"}"#;
        let fields = [field(
            &[Key("v")],
            FieldType::String,
            Some(r"^x\n(?<tail>y)$"),
            false,
        )];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state
            .result
            .capture(captures[&(0, 0, Some("tail".to_owned()))])
        {
            CaptureValue::PredicateCapture(s @ UnescapedString::Owned(_)) => {
                assert_eq!(s.resolve(input), "y");
            }
            other => panic!("expected Owned predicate capture, got {other:?}"),
        }
    }

    #[test]
    fn predicate_on_number_raw_text() {
        let fields = [field(
            &[Key("n")],
            FieldType::Number,
            Some(r"^-?\d+\.\d+$"),
            false,
        )];
        let (machine, _) = compile(&[&fields]);
        assert!(run(&machine, r#"{"n":3.25}"#).unwrap().result.did_match(0));
        assert!(!run(&machine, r#"{"n":325}"#).unwrap().result.did_match(0));
    }

    #[test]
    fn multiple_sets_independent() {
        let a = [field(&[Key("a")], FieldType::Number, None, false)];
        let b = [field(&[Key("a")], FieldType::String, None, false)];
        let empty: [FieldMatch<'_>; 0] = [];
        let (machine, _) = compile(&[&a, &b, &empty]);
        assert_eq!(machine.num_match_sets(), 3);
        let state = run(&machine, r#"{"a":1}"#).unwrap();
        assert!(state.result.did_match(0));
        assert!(!state.result.did_match(1));
        // A set with no fields is vacuously satisfied.
        assert!(state.result.did_match(2));
    }

    #[test]
    fn partial_set_does_not_match() {
        let fields = [
            field(&[Key("a")], FieldType::Number, None, false),
            field(&[Key("missing")], FieldType::Number, None, false),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(!run(&machine, r#"{"a":1}"#).unwrap().result.did_match(0));
    }

    #[test]
    fn duplicate_keys_first_satisfying_wins() {
        let fields = [field(&[Key("a")], FieldType::String, Some("^[yz]"), true)];
        let (machine, captures) = compile(&[&fields]);
        // First occurrence fails the predicate, second satisfies.
        let input = r#"{"a":"x","a":"y"}"#;
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 0, None)]) {
            CaptureValue::String(s) => assert_eq!(s.resolve(input), "y"),
            other => panic!("expected String, got {other:?}"),
        }
        // Both satisfy: the first one provides the capture.
        let input = r#"{"a":"y1","a":"z2"}"#;
        let state = run(&machine, input).unwrap();
        match state.result.capture(captures[&(0, 0, None)]) {
            CaptureValue::String(s) => assert_eq!(s.resolve(input), "y1"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn fixed_index() {
        let fields = [field(&[Key("a"), Index(1)], FieldType::Number, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, r#"{"a":[10,42,99]}"#).unwrap();
        assert!(state.result.did_match(0));
        assert_eq!(
            *state.result.capture(captures[&(0, 0, None)]),
            CaptureValue::Number(42.0)
        );
        assert!(!run(&machine, r#"{"a":[10]}"#).unwrap().result.did_match(0));
        assert!(
            !run(&machine, r#"{"a":{"1":42}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
    }

    #[test]
    fn any_index_first_satisfying() {
        let input = r#"{"a":["x","bb","ccc","dd"]}"#;
        let fields = [field(
            &[Key("a"), AnyIndex],
            FieldType::String,
            Some("^..$"),
            true,
        )];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 0, None)]) {
            CaptureValue::String(s) => assert_eq!(s.resolve(input), "bb"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn any_index_skips_wrong_types() {
        let fields = [field(
            &[Key("a"), AnyIndex, Key("id")],
            FieldType::Number,
            None,
            true,
        )];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, r#"{"a":[1,"s",{"other":0},{"id":5}]}"#).unwrap();
        assert!(state.result.did_match(0));
        assert_eq!(
            *state.result.capture(captures[&(0, 0, None)]),
            CaptureValue::Number(5.0)
        );
    }

    #[test]
    fn nested_wildcards() {
        let fields = [field(
            &[Key("a"), AnyIndex, AnyIndex],
            FieldType::Number,
            None,
            true,
        )];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, r#"{"a":[["x"],[null,7]]}"#).unwrap();
        assert!(state.result.did_match(0));
        assert_eq!(
            *state.result.capture(captures[&(0, 0, None)]),
            CaptureValue::Number(7.0)
        );
    }

    #[test]
    fn wildcard_merged_into_fixed_index() {
        // The satisfying element for the wildcard field sits AT a fixed index;
        // it is only found because the wildcard subtree is merged into the
        // fixed-index subtree at compile time.
        let input = r#"{"a":["s",5]}"#;
        let fields = [
            field(&[Key("a"), Index(0)], FieldType::Any, None, false),
            field(&[Key("a"), AnyIndex], FieldType::String, None, true),
        ];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 1, None)]) {
            CaptureValue::String(s) => assert_eq!(s.resolve(input), "s"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn container_mismatch_leaves_unsatisfied() {
        let fields = [field(&[Key("a"), Key("b")], FieldType::Number, None, false)];
        let (machine, _) = compile(&[&fields]);
        assert!(!run(&machine, r#"{"a":[1,2]}"#).unwrap().result.did_match(0));
        assert!(!run(&machine, r#"{"a":"b"}"#).unwrap().result.did_match(0));
    }

    #[test]
    fn empty_path_matches_root() {
        let input = r#"  {"k":1}  "#;
        let fields = [field(&[], FieldType::Object, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 0, None)]) {
            CaptureValue::Object(range) => {
                assert_eq!(&input[range_u32_to_usize(*range)], r#"{"k":1}"#)
            }
            other => panic!("expected Object, got {other:?}"),
        }
    }

    #[test]
    fn root_array() {
        let fields = [field(&[Index(1)], FieldType::Number, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, "[1, 2]").unwrap();
        assert!(state.result.did_match(0));
        assert_eq!(
            *state.result.capture(captures[&(0, 0, None)]),
            CaptureValue::Number(2.0)
        );
    }

    #[test]
    fn malformed_inputs_error() {
        let fields = [field(&[Key("a")], FieldType::Any, None, false)];
        let (machine, _) = compile(&[&fields]);
        assert!(matches!(
            run(&machine, r#"{"a":tru}"#),
            Err(MatchError::UnexpectedByte { .. })
        ));
        assert!(matches!(
            run(&machine, r#"{"a":1"#),
            Err(MatchError::UnexpectedEof)
        ));
        assert!(matches!(
            run(&machine, "[1,2"),
            Err(MatchError::UnexpectedEof)
        ));
        assert!(matches!(
            run(&machine, r#"{"a":1,}"#),
            Err(MatchError::UnexpectedByte { .. })
        ));
        assert!(matches!(
            run(&machine, "{} x"),
            Err(MatchError::TrailingData { pos: 3 })
        ));
        assert!(matches!(run(&machine, ""), Err(MatchError::UnexpectedEof)));
        // The whole input is validated, so bad escapes error even in values
        // that are never captured or unescaped.
        assert!(matches!(
            run(&machine, r#"{"a":"\q"}"#),
            Err(MatchError::InvalidEscape { .. })
        ));
        assert!(matches!(
            run(&machine, r#"{"a":"\ud83d oops"}"#),
            Err(MatchError::InvalidEscape { .. })
        ));
    }

    #[test]
    fn skipped_regions_are_validated() {
        // "a" is the only pattern path; everything else is skipped — and still
        // must be JSON-compliant.
        let fields = [field(&[Key("a")], FieldType::Any, None, false)];
        let (machine, _) = compile(&[&fields]);
        for bad in [
            r#"{"noise":01,"a":1}"#,         // leading zero
            r#"{"noise":1.,"a":1}"#,         // bare decimal point
            r#"{"noise":1e,"a":1}"#,         // missing exponent digits
            r#"{"noise":+1,"a":1}"#,         // leading plus
            r#"{"noise":--1,"a":1}"#,        // double sign
            r#"{"noise":[1,2},"a":1}"#,      // bracket mismatch
            r#"{"noise":{"x"},"a":1}"#,      // missing colon
            r#"{"noise":[1,],"a":1}"#,       // trailing comma
            r#"{"noise":{"x":},"a":1}"#,     // missing member value
            r#"{"noise":"\q","a":1}"#,       // bad escape in skipped string
            r#"{"noise":"\udc00","a":1}"#,   // lone low surrogate
            "{\"noise\":\"\u{1}\",\"a\":1}", // raw control character
            r#"{"noise":[nul],"a":1}"#,      // bad keyword
        ] {
            assert!(run(&machine, bad).is_err(), "should error: {bad}");
        }
        for good in [
            r#"{"noise":[1,-0.5e+10,{"x":[[]],"y":"A"},true],"a":1}"#,
            r#"{"noise":0.125,"a":1}"#,
            r#"{"noise":-0,"a":1}"#,
        ] {
            assert!(
                run(&machine, good).unwrap().result.did_match(0),
                "should match: {good}"
            );
        }
    }

    #[test]
    fn depth_limit() {
        let fields = [field(&[Key("a")], FieldType::Any, None, false)];
        let (machine, _) = compile(&[&fields]);
        let deep = |n: usize| format!("{}0{}", "[".repeat(n), "]".repeat(n));

        let mut state = machine.allocate_state();
        assert_eq!(state.depth_limit(), DEFAULT_DEPTH_LIMIT);
        machine.match_string(&deep(128), &mut state).unwrap();
        assert!(matches!(
            machine.match_string(&deep(129), &mut state),
            Err(MatchError::DepthLimitExceeded { .. })
        ));

        state.set_depth_limit(4);
        machine.match_string(&deep(4), &mut state).unwrap();
        assert!(matches!(
            machine.match_string(&deep(5), &mut state),
            Err(MatchError::DepthLimitExceeded { .. })
        ));
        // Deep nesting inside a skipped member is limited the same way.
        assert!(matches!(
            machine.match_string(&format!(r#"{{"noise":{},"a":1}}"#, deep(5)), &mut state),
            Err(MatchError::DepthLimitExceeded { .. })
        ));

        state.set_depth_limit(300);
        machine.match_string(&deep(300), &mut state).unwrap();
    }

    #[test]
    fn unicode_escapes() {
        let input = "{\"s\":\"\\u0041\\ud83d\\ude00\"}";
        let fields = [field(&[Key("s")], FieldType::String, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        match state.result.capture(captures[&(0, 0, None)]) {
            CaptureValue::String(s) => assert_eq!(s.resolve(input), "A😀"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn whitespace_tolerated() {
        let fields = [field(&[Key("a"), Key("b")], FieldType::Number, None, true)];
        let (machine, _) = compile(&[&fields]);
        let state = run(&machine, "\n{ \"a\" : { \"b\" :\t42 } }\r\n").unwrap();
        assert!(state.result.did_match(0));
    }

    #[test]
    fn state_reuse_resets_everything() {
        let fields = [field(&[Key("a")], FieldType::Number, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let mut state = machine.allocate_state();
        machine.match_string(r#"{"a":1}"#, &mut state).unwrap();
        assert!(state.result.did_match(0));
        machine.match_string(r#"{"b":1}"#, &mut state).unwrap();
        assert!(!state.result.did_match(0));
        assert_eq!(
            *state.result.capture(captures[&(0, 0, None)]),
            CaptureValue::NotCaptured
        );
    }

    #[test]
    fn capture_callback_ordering() {
        let set0 = [
            field(
                &[Key("a")],
                FieldType::String,
                Some("(?<g1>x)(?<_skip>y)(?<g2>z)"),
                true,
            ),
            field(&[Key("b")], FieldType::Number, None, false),
            field(&[Key("c")], FieldType::Number, None, true),
        ];
        let set1 = [field(&[Key("d")], FieldType::Number, None, true)];
        let mut seen: Vec<(u32, u32, Option<String>, u32, u32)> = Vec::new();
        MatchMachine::compile(
            [
                MatchSet {
                    field_matches: &set0,
                },
                MatchSet {
                    field_matches: &set1,
                },
            ]
            .into_iter(),
            |args| {
                seen.push((
                    args.match_set_index,
                    args.field_index,
                    args.predicate_capture_name.map(str::to_owned),
                    args.capture_index_in_set,
                    args.capture_index_in_machine,
                ));
            },
        )
        .unwrap();
        assert_eq!(
            seen,
            vec![
                (0, 0, None, 0, 0),
                (0, 0, Some("g1".to_owned()), 1, 1),
                (0, 0, Some("g2".to_owned()), 2, 2),
                (0, 2, None, 3, 3),
                (1, 0, None, 0, 4),
            ]
        );
    }

    #[test]
    fn oracle_against_serde_json() {
        let fields = test_fields();
        let (machine, captures) = compile(&[&fields]);
        let mut state = machine.allocate_state();
        let mut rng = StdRng::seed_from_u64(0xB00B5);
        for round in 0..50 {
            let bloat = (round % 5) as f64;
            let json = generate_test_json(&fields, 1.0, bloat, &mut rng);
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            machine.match_string(&json, &mut state).unwrap();
            assert!(state.result.did_match(0), "should match: {json}");

            for (field_index, field) in fields.iter().enumerate() {
                let capture = state
                    .result
                    .capture(captures[&(0, field_index as u32, None)]);
                // Navigate serde's tree along deterministic segments.
                let mut value = Some(&parsed);
                for segment in field.path {
                    value = match (segment, value) {
                        (PathSegment::Key(key), Some(v)) => v.get(*key),
                        (PathSegment::Index(index), Some(v)) => v.get(*index as usize),
                        (PathSegment::AnyIndex, _) => {
                            value = None;
                            break;
                        }
                        _ => None,
                    };
                }
                match (&field.r#type, value) {
                    (FieldType::String, Some(expected)) => match capture {
                        CaptureValue::String(s) => {
                            assert_eq!(s.resolve(&json), expected.as_str().unwrap());
                        }
                        other => panic!("expected String, got {other:?}"),
                    },
                    (FieldType::Bool, Some(expected)) => {
                        assert_eq!(*capture, CaptureValue::Bool(expected.as_bool().unwrap()));
                    }
                    (FieldType::Object, Some(expected)) => match capture {
                        CaptureValue::Object(range) => {
                            let reparsed: serde_json::Value =
                                serde_json::from_str(&json[range_u32_to_usize(*range)]).unwrap();
                            assert_eq!(&reparsed, expected);
                        }
                        other => panic!("expected Object, got {other:?}"),
                    },
                    (FieldType::Number, None) => {
                        // AnyIndex path: just require that a number was captured.
                        assert!(matches!(capture, CaptureValue::Number(_)));
                    }
                    (ty, value) => {
                        panic!("unhandled oracle case: {ty:?} vs {value:?}");
                    }
                }
            }
        }
        // Empty documents must not match but must not error either.
        for _ in 0..10 {
            let json = generate_test_json(&fields, 0.0, 3.0, &mut rng);
            machine.match_string(&json, &mut state).unwrap();
            assert!(!state.result.did_match(0), "should not match: {json}");
        }
    }
    #[test]
    fn print_generater_json() {
        let mut rng = StdRng::seed_from_u64(0xB00B5);
        let json = generate_test_json(&test_fields(), 1.0, 64.0, &mut rng);
        println!("{}", json);
    }
}
