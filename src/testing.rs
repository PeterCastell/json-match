//! Random test-JSON generation and the crate's test suite.
//!
//! `generate_test_json` materializes JSON that satisfies a chosen fraction of a
//! pattern's fields (`closeness`) surrounded by structured noise (`bloat`).
//! Predicates are ignored by the generator: a "satisfying" value has the right
//! path and type, so patterns fed to it should be predicate-free (or use
//! predicates that accept any generated value).

use compact_str::CompactString;
use rand::distr::Alphanumeric;
use rand::rngs::StdRng;
use rand::{Rng, RngExt};

use crate::{FieldPattern, FieldType, NodeId, PathSegment};

#[derive(Default)]
struct GenNode<'a> {
    key_children: Vec<(&'a str, NodeId)>,
    index_children: Vec<(u16, NodeId)>,
    any_index_child: Option<NodeId>,
    terminals: Vec<&'a FieldType>,
}

fn gen_child<'a>(nodes: &mut Vec<GenNode<'a>>, parent: NodeId, segment: &'a PathSegment) -> NodeId {
    fn push<'a>(nodes: &mut Vec<GenNode<'a>>) -> NodeId {
        nodes.push(GenNode::default());
        NodeId((nodes.len() - 1).try_into().unwrap())
    }
    match segment {
        PathSegment::Key(key) => {
            let key = key.as_str();
            if let Some(&(_, child)) = nodes[parent.index()]
                .key_children
                .iter()
                .find(|&&(k, _)| k == key)
            {
                child
            } else {
                let child = push(nodes);
                nodes[parent.index()].key_children.push((key, child));
                child
            }
        }
        PathSegment::Index(index) => {
            if let Some(&(_, child)) = nodes[parent.index()]
                .index_children
                .iter()
                .find(|&&(i, _)| i == *index)
            {
                child
            } else {
                let child = push(nodes);
                nodes[parent.index()].index_children.push((*index, child));
                child
            }
        }
        PathSegment::AnyIndex => match nodes[parent.index()].any_index_child {
            Some(child) => child,
            None => {
                let child = push(nodes);
                nodes[parent.index()].any_index_child = Some(child);
                child
            }
        },
    }
}

pub fn generate_test_json(
    fields: &[FieldPattern<'_>],
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
        let mut node = NodeId(0);
        for segment in field.path {
            node = gen_child(&mut nodes, node, segment);
        }
        nodes[node.index()].terminals.push(&field.r#type);
    }

    let mut emitter = Emitter {
        nodes: &nodes,
        rng,
        bloat,
    };
    emitter.value(NodeId(0), 1)
}

struct Emitter<'n, 'a, 'r> {
    nodes: &'n [GenNode<'a>],
    rng: &'r mut StdRng,
    bloat: f64,
}

impl Emitter<'_, '_, '_> {
    fn value(&mut self, node: NodeId, level: usize) -> String {
        // Structure demanded by children takes priority over terminal types:
        // a node used as a path prefix must be a container.
        if !self.nodes[node.index()].key_children.is_empty() {
            self.object(node, level)
        } else if !self.nodes[node.index()].index_children.is_empty()
            || self.nodes[node.index()].any_index_child.is_some()
        {
            self.array(node, level)
        } else if let Some(ty) = self.nodes[node.index()].terminals.first() {
            self.terminal(ty)
        } else {
            self.primitive()
        }
    }

    fn object(&mut self, node: NodeId, level: usize) -> String {
        let real_keys: Vec<&str> = self.nodes[node.index()]
            .key_children
            .iter()
            .map(|&(k, _)| k)
            .collect();
        let mut members: Vec<String> = Vec::new();
        for i in 0..self.nodes[node.index()].key_children.len() {
            let (name, child) = self.nodes[node.index()].key_children[i];
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

    fn array(&mut self, node: NodeId, level: usize) -> String {
        let base_len = self.nodes[node.index()]
            .index_children
            .iter()
            .map(|&(i, _)| i as usize + 1)
            .max()
            .unwrap_or(0);
        let extra = usize::from(self.nodes[node.index()].any_index_child.is_some())
            + (self.bloat * 2.0).floor() as usize;
        let mut slots: Vec<Option<NodeId>> = vec![None; base_len + extra];
        for i in 0..self.nodes[node.index()].index_children.len() {
            let (index, child) = self.nodes[node.index()].index_children[i];
            slots[index as usize] = Some(child);
        }
        if let Some(any_child) = self.nodes[node.index()].any_index_child {
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
            FieldType::String => serde_json::to_string(&rand_string(self.rng)).unwrap(),
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
            0 => serde_json::to_string(&rand_string(self.rng)).unwrap(),
            1 => noise_number(self.rng),
            2 => self.rng.random_bool(0.5).to_string(),
            _ => "null".to_string(),
        }
    }
}

fn rand_name(rng: &mut StdRng) -> String {
    let len = rng.random_range(1..=16);
    (0..len)
        .map(|_| char::from(rng.sample(Alphanumeric)))
        .collect()
}

fn rand_string(rng: &mut StdRng) -> String {
    let len = rng.random_range(1..=128);
    (0..len)
        .map(|_| {
            if rng.random_bool(0.001) {
                rng.sample(rand::distr::StandardUniform::default())
            } else if rng.random_bool(0.01) {
                *rng.sample(rand::distr::slice::Choose::new(b"\n\r\t ").unwrap()) as char
            } else {
                char::from(rng.sample(Alphanumeric))
            }
        })
        .collect()
}

fn noise_number(rng: &mut impl Rng) -> String {
    if rng.random_bool(0.5) {
        rng.random_range(-1_000..1_000i64).to_string()
    } else {
        format!("{:.2}", rng.random_range(-1_000.0..1_000.0f64))
    }
}

/// A representative predicate-free pattern, shared by tests and benches.
pub fn test_fields() -> Vec<FieldPattern<'static>> {
    use PathSegment::{AnyIndex, Index, Key};
    const fn key(s: &'static str) -> PathSegment {
        Key(CompactString::const_new(s))
    }
    static FOO: [PathSegment; 1] = [key("foo")];
    static A_B: [PathSegment; 2] = [key("a"), key("b")];
    static A_C: [PathSegment; 2] = [key("a"), key("c")];
    static A_C_D: [PathSegment; 3] = [key("a"), key("c"), key("d")];
    static LIST_0_NAME: [PathSegment; 3] = [key("list"), Index(0), key("name")];
    static LIST_ANY_ID: [PathSegment; 3] = [key("list"), AnyIndex, key("id")];
    let plain = |path: &'static [PathSegment], r#type: FieldType| FieldPattern {
        path,
        r#type,
        predicate: None,
        capture: true,
        exhaustive: false,
    };
    vec![
        plain(&FOO, FieldType::String),
        plain(&A_B, FieldType::Bool),
        plain(&A_C, FieldType::Object),
        plain(&A_C_D, FieldType::String),
        plain(&LIST_0_NAME, FieldType::String),
        plain(&LIST_ANY_ID, FieldType::Number),
    ]
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use crate::{
        CaptureCallbackArgs, CaptureValue, CompileError, DEFAULT_DEPTH_LIMIT, FieldPattern,
        FieldType, MachineCaptureIndex, MachineState, MatchError, MatchMachine, PathSegment,
        Pattern, SetCaptureIndex, UnescapedString, range_u32_to_usize,
        testing::{generate_test_json, test_fields},
    };

    /// Machine capture indices keyed by (set index, field index, predicate group name).
    type CaptureIndices =
        std::collections::HashMap<(u32, u32, Option<String>), MachineCaptureIndex>;

    fn compile_err(sets: &[&[FieldPattern<'_>]]) -> CompileError {
        MatchMachine::compile(sets.iter().map(|fields| Pattern { fields }), |_| {}).unwrap_err()
    }

    fn compile(sets: &[&[FieldPattern<'_>]]) -> (MatchMachine, CaptureIndices) {
        let mut captures = std::collections::HashMap::new();
        let machine = MatchMachine::compile(sets.iter().map(|fields| Pattern { fields }), |args| {
            captures.insert(
                (
                    args.match_set_index,
                    args.field_index,
                    args.predicate_capture_name.map(str::to_owned),
                ),
                args.capture_index_in_machine,
            );
        })
        .unwrap();
        (machine, captures)
    }

    fn run(machine: &MatchMachine, input: &str) -> Result<MachineState, MatchError> {
        let mut state = machine.allocate_state();
        machine.match_string(input, &mut state)?;
        Ok(state)
    }

    /// Leaks the path to satisfy FieldMatch's borrowed slice; fine in tests.
    fn field(
        path: Vec<PathSegment>,
        r#type: FieldType,
        predicate: Option<&str>,
        capture: bool,
    ) -> FieldPattern<'static> {
        FieldPattern {
            path: Vec::leak(path),
            r#type,
            predicate: predicate.map(|p| Regex::new(p).unwrap()),
            capture,
            exhaustive: false,
        }
    }

    /// `field` with `exhaustive: true`.
    fn xfield(path: Vec<PathSegment>, r#type: FieldType, capture: bool) -> FieldPattern<'static> {
        FieldPattern {
            exhaustive: true,
            ..field(path, r#type, None, capture)
        }
    }

    fn key(s: &str) -> PathSegment {
        PathSegment::Key(s.into())
    }

    use PathSegment::*;
    use rand::{SeedableRng, rngs::StdRng};
    use regex::Regex;

    #[test]
    fn basic_nested_capture() {
        let fields = [field(vec![key("a"), key("b")], FieldType::Bool, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, r#"{"x":1,"a":{"y":[3],"b":true},"z":null}"#).unwrap();
        assert!(state.result.did_match(0));
        assert_matches!(
            state.result.capture(captures[&(0, 0, None)]),
            Some(CaptureValue::Bool(true))
        );
    }

    #[test]
    fn type_mismatch_no_match() {
        let fields = [field(vec![key("a")], FieldType::Number, None, false)];
        let (machine, _) = compile(&[&fields]);
        let state = run(&machine, r#"{"a":"not a number"}"#).unwrap();
        assert!(!state.result.did_match(0));
    }

    #[test]
    fn all_capture_types() {
        let input = r#"{"o":{"k":1},"ar":[1,2],"s":"hi","n":-12.5e2,"b":false,"nul":null}"#;
        let fields = [
            field(vec![key("o")], FieldType::Object, None, true),
            field(vec![key("ar")], FieldType::Array, None, true),
            field(vec![key("s")], FieldType::String, None, true),
            field(vec![key("n")], FieldType::Number, None, true),
            field(vec![key("b")], FieldType::Bool, None, true),
            field(vec![key("nul")], FieldType::Null, None, true),
        ];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 0, None)]) {
            Some(CaptureValue::Object(range)) => {
                assert_eq!(&input[range_u32_to_usize(*range)], r#"{"k":1}"#)
            }
            other => panic!("expected Object, got {other:?}"),
        }
        match state.result.capture(captures[&(0, 1, None)]) {
            Some(CaptureValue::Array(range)) => {
                assert_eq!(&input[range_u32_to_usize(*range)], "[1,2]")
            }
            other => panic!("expected Array, got {other:?}"),
        }
        match state.result.capture(captures[&(0, 2, None)]) {
            Some(CaptureValue::String(s)) => assert_eq!(s.resolve(input), "hi"),
            other => panic!("expected String, got {other:?}"),
        }
        assert_matches!(
            state.result.capture(captures[&(0, 3, None)]),
            Some(CaptureValue::Number(-1250.0))
        );
        assert_matches!(
            state.result.capture(captures[&(0, 4, None)]),
            Some(CaptureValue::Bool(false))
        );
        assert_matches!(
            state.result.capture(captures[&(0, 5, None)]),
            Some(CaptureValue::Null)
        );
    }

    #[test]
    fn literal_and_any() {
        let input = r#"{"lit":[1,2,3],"any":{"deep":1}}"#;
        let fields = [
            field(
                vec![key("lit")],
                FieldType::Literal("[1,2,3]".into()),
                None,
                true,
            ),
            field(vec![key("any")], FieldType::Any, None, true),
        ];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 1, None)]) {
            Some(CaptureValue::Object(range)) => {
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
        let fields = [field(vec![key("s")], FieldType::String, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        match state.result.capture(captures[&(0, 0, None)]) {
            Some(CaptureValue::String(UnescapedString::Borrowed(range))) => {
                assert_eq!(&input[range_u32_to_usize(*range)], "plain");
            }
            other => panic!("expected Borrowed, got {other:?}"),
        }
    }

    #[test]
    fn escaped_string_capture_owned() {
        let input = r#"{"s":"line1\nline2 A 😀 \\"}"#;
        let fields = [field(vec![key("s")], FieldType::String, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        match state.result.capture(captures[&(0, 0, None)]) {
            Some(CaptureValue::String(s @ UnescapedString::Owned(_))) => {
                assert_eq!(s.resolve(input), "line1\nline2 A 😀 \\");
            }
            other => panic!("expected Owned, got {other:?}"),
        }
    }

    #[test]
    fn escaped_key_lookup() {
        let fields = [field(vec![key("a\nb")], FieldType::Number, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, r#"{"a\nb":7}"#).unwrap();
        assert!(state.result.did_match(0));
        assert_matches!(
            state.result.capture(captures[&(0, 0, None)]),
            Some(CaptureValue::Number(7.0))
        );
        // \u escape spelling of a key must also resolve.
        let fields = [field(vec![key("A")], FieldType::Number, None, false)];
        let (machine, _) = compile(&[&fields]);
        assert!(run(&machine, r#"{"A":1}"#).unwrap().result.did_match(0));
    }

    #[test]
    fn predicate_gates_match_and_captures_groups() {
        let input = r#"{"v":"hello-42"}"#;
        let fields = [field(
            vec![key("v")],
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
            Some(CaptureValue::PredicateCapture(UnescapedString::Borrowed(range))) => {
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
            vec![key("v")],
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
            Some(CaptureValue::PredicateCapture(s @ UnescapedString::Owned(_))) => {
                assert_eq!(s.resolve(input), "y");
            }
            other => panic!("expected Owned predicate capture, got {other:?}"),
        }
    }

    #[test]
    fn predicate_on_number_raw_text() {
        let fields = [field(
            vec![key("n")],
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
        let a = [field(vec![key("a")], FieldType::Number, None, false)];
        let b = [field(vec![key("a")], FieldType::String, None, false)];
        let empty: [FieldPattern<'_>; 0] = [];
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
            field(vec![key("a")], FieldType::Number, None, false),
            field(vec![key("missing")], FieldType::Number, None, false),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(!run(&machine, r#"{"a":1}"#).unwrap().result.did_match(0));
    }

    #[test]
    fn duplicate_keys_first_satisfying_wins() {
        let fields = [field(
            vec![key("a")],
            FieldType::String,
            Some("^[yz]"),
            true,
        )];
        let (machine, captures) = compile(&[&fields]);
        // First occurrence fails the predicate, second satisfies.
        let input = r#"{"a":"x","a":"y"}"#;
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 0, None)]) {
            Some(CaptureValue::String(s)) => assert_eq!(s.resolve(input), "y"),
            other => panic!("expected String, got {other:?}"),
        }
        // Both satisfy: the first one provides the capture.
        let input = r#"{"a":"y1","a":"z2"}"#;
        let state = run(&machine, input).unwrap();
        match state.result.capture(captures[&(0, 0, None)]) {
            Some(CaptureValue::String(s)) => assert_eq!(s.resolve(input), "y1"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn fixed_index() {
        let fields = [field(
            vec![key("a"), Index(1)],
            FieldType::Number,
            None,
            true,
        )];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, r#"{"a":[10,42,99]}"#).unwrap();
        assert!(state.result.did_match(0));
        assert_matches!(
            state.result.capture(captures[&(0, 0, None)]),
            Some(CaptureValue::Number(42.0))
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
            vec![key("a"), AnyIndex],
            FieldType::String,
            Some("^..$"),
            true,
        )];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 0, None)]) {
            Some(CaptureValue::String(s)) => assert_eq!(s.resolve(input), "bb"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn any_index_skips_wrong_types() {
        let fields = [field(
            vec![key("a"), AnyIndex, key("id")],
            FieldType::Number,
            None,
            true,
        )];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, r#"{"a":[1,"s",{"other":0},{"id":5}]}"#).unwrap();
        assert!(state.result.did_match(0));
        assert_matches!(
            state.result.capture(captures[&(0, 0, None)]),
            Some(CaptureValue::Number(5.0))
        );
    }

    #[test]
    fn nested_wildcards() {
        let fields = [field(
            vec![key("a"), AnyIndex, AnyIndex],
            FieldType::Number,
            None,
            true,
        )];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, r#"{"a":[["x"],[null,7]]}"#).unwrap();
        assert!(state.result.did_match(0));
        assert_matches!(
            state.result.capture(captures[&(0, 0, None)]),
            Some(CaptureValue::Number(7.0))
        );
    }

    #[test]
    fn wildcard_merged_into_fixed_index() {
        // The satisfying element for the wildcard field sits AT a fixed index;
        // it is only found because the wildcard subtree is merged into the
        // fixed-index subtree at compile time.
        let input = r#"{"a":["s",5]}"#;
        let fields = [
            field(vec![key("a"), Index(0)], FieldType::Any, None, false),
            field(vec![key("a"), AnyIndex], FieldType::String, None, true),
        ];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 1, None)]) {
            Some(CaptureValue::String(s)) => assert_eq!(s.resolve(input), "s"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn container_mismatch_leaves_unsatisfied() {
        let fields = [field(
            vec![key("a"), key("b")],
            FieldType::Number,
            None,
            false,
        )];
        let (machine, _) = compile(&[&fields]);
        assert!(!run(&machine, r#"{"a":[1,2]}"#).unwrap().result.did_match(0));
        assert!(!run(&machine, r#"{"a":"b"}"#).unwrap().result.did_match(0));
    }

    #[test]
    fn empty_path_matches_root() {
        let input = r#"  {"k":1}  "#;
        let fields = [field(vec![], FieldType::Object, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 0, None)]) {
            Some(CaptureValue::Object(range)) => {
                assert_eq!(&input[range_u32_to_usize(*range)], r#"{"k":1}"#)
            }
            other => panic!("expected Object, got {other:?}"),
        }
    }

    #[test]
    fn root_array() {
        let fields = [field(vec![Index(1)], FieldType::Number, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, "[1, 2]").unwrap();
        assert!(state.result.did_match(0));
        assert_matches!(
            state.result.capture(captures[&(0, 0, None)]),
            Some(CaptureValue::Number(2.0))
        );
    }

    #[test]
    fn malformed_inputs_error() {
        let fields = [field(vec![key("a")], FieldType::Any, None, false)];
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
        let fields = [field(vec![key("a")], FieldType::Any, None, false)];
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
        let fields = [field(vec![key("a")], FieldType::Any, None, false)];
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
    fn number_overflow_errors() {
        // JSON has no infinity: captured literals beyond the finite f64 range
        // fail the match, agreeing with serde_json ("number out of range").
        let fields = [field(vec![key("n")], FieldType::Number, None, true)];
        let (machine, _) = compile(&[&fields]);
        assert!(matches!(
            run(&machine, r#"{"n":1e999}"#),
            Err(MatchError::NumberOutOfRange { pos: 5 })
        ));
        assert!(matches!(
            run(&machine, r#"{"n":-1e999}"#),
            Err(MatchError::NumberOutOfRange { pos: 5 })
        ));
        // Underflow to zero and subnormals are fine.
        let state = run(&machine, r#"{"n":1e-999}"#).unwrap();
        assert!(state.result.did_match(0));
        // Uncaptured numbers are validated syntactically but never parsed,
        // so no range check applies.
        let fields = [field(vec![key("n")], FieldType::Number, None, false)];
        let (machine, _) = compile(&[&fields]);
        assert!(run(&machine, r#"{"n":1e999}"#).unwrap().result.did_match(0));
    }

    #[test]
    fn predicate_is_unanchored() {
        // Predicates are substring matches unless the pattern anchors itself.
        let fields = [field(vec![key("v")], FieldType::String, Some("bb"), false)];
        let (machine, _) = compile(&[&fields]);
        assert!(
            run(&machine, r#"{"v":"aaabbccc"}"#)
                .unwrap()
                .result
                .did_match(0)
        );
    }

    #[test]
    fn control_character_boundaries() {
        let fields = [field(vec![key("v")], FieldType::String, None, true)];
        let (machine, captures) = compile(&[&fields]);
        // Escaped NUL is legal JSON.
        let state = run(&machine, r#"{"v":"\u0000x"}"#).unwrap();
        match state.result.capture(captures[&(0, 0, None)]) {
            Some(CaptureValue::String(s)) => assert_eq!(s.resolve(r#"{"v":"\u0000x"}"#), "\0x"),
            other => panic!("expected String, got {other:?}"),
        }
        // Raw DEL (0x7F) is legal; only 0x00..=0x1F are rejected unescaped.
        let input = "{\"v\":\"\u{7f}\"}";
        assert!(run(&machine, input).unwrap().result.did_match(0));
    }

    #[test]
    fn deep_pattern_path() {
        // Nesting along matched paths recurses on the call stack, bounded by
        // pattern depth rather than the state's depth limit.
        let path: Vec<PathSegment> = (0..1000).map(|_| key("k")).collect();
        let fields = [field(path, FieldType::Number, None, false)];
        let (machine, _) = compile(&[&fields]);
        let json = format!("{}1{}", "{\"k\":".repeat(1000), "}".repeat(1000));
        assert!(run(&machine, &json).unwrap().result.did_match(0));
    }

    #[test]
    fn unicode_escapes() {
        let input = "{\"s\":\"\\u0041\\ud83d\\ude00\"}";
        let fields = [field(vec![key("s")], FieldType::String, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let state = run(&machine, input).unwrap();
        match state.result.capture(captures[&(0, 0, None)]) {
            Some(CaptureValue::String(s)) => assert_eq!(s.resolve(input), "A😀"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn whitespace_tolerated() {
        let fields = [field(
            vec![key("a"), key("b")],
            FieldType::Number,
            None,
            true,
        )];
        let (machine, _) = compile(&[&fields]);
        let state = run(&machine, "\n{ \"a\" : { \"b\" :\t42 } }\r\n").unwrap();
        assert!(state.result.did_match(0));
    }

    #[test]
    fn state_reuse_resets_everything() {
        let fields = [field(vec![key("a")], FieldType::Number, None, true)];
        let (machine, captures) = compile(&[&fields]);
        let mut state = machine.allocate_state();
        machine.match_string(r#"{"a":1}"#, &mut state).unwrap();
        assert!(state.result.did_match(0));
        machine.match_string(r#"{"b":1}"#, &mut state).unwrap();
        assert!(!state.result.did_match(0));
        assert_matches!(state.result.capture(captures[&(0, 0, None)]), None);
    }

    #[test]
    fn capture_callback_ordering() {
        let set0 = [
            field(
                vec![key("a")],
                FieldType::String,
                Some("(?<g1>x)(?<_skip>y)(?<g2>z)"),
                true,
            ),
            field(vec![key("b")], FieldType::Number, None, false),
            field(vec![key("c")], FieldType::Number, None, true),
        ];
        let set1 = [field(vec![key("d")], FieldType::Number, None, true)];
        let mut seen: Vec<CaptureCallbackArgs> = Vec::new();
        MatchMachine::compile(
            [Pattern { fields: &set0 }, Pattern { fields: &set1 }].into_iter(),
            |args| {
                seen.push(args);
            },
        )
        .unwrap();
        assert_eq!(
            seen,
            &[
                CaptureCallbackArgs {
                    match_set_index: 0,
                    field_index: 0,
                    predicate_capture_name: None,
                    capture_index_in_set: SetCaptureIndex::new(0).unwrap(),
                    capture_index_in_machine: MachineCaptureIndex::new(0).unwrap(),
                },
                CaptureCallbackArgs {
                    match_set_index: 0,
                    field_index: 0,
                    predicate_capture_name: Some("g1"),
                    capture_index_in_set: SetCaptureIndex::new(1).unwrap(),
                    capture_index_in_machine: MachineCaptureIndex::new(1).unwrap(),
                },
                CaptureCallbackArgs {
                    match_set_index: 0,
                    field_index: 0,
                    predicate_capture_name: Some("g2"),
                    capture_index_in_set: SetCaptureIndex::new(2).unwrap(),
                    capture_index_in_machine: MachineCaptureIndex::new(2).unwrap(),
                },
                CaptureCallbackArgs {
                    match_set_index: 0,
                    field_index: 2,
                    predicate_capture_name: None,
                    capture_index_in_set: SetCaptureIndex::new(3).unwrap(),
                    capture_index_in_machine: MachineCaptureIndex::new(3).unwrap(),
                },
                CaptureCallbackArgs {
                    match_set_index: 1,
                    field_index: 0,
                    predicate_capture_name: None,
                    capture_index_in_set: SetCaptureIndex::new(0).unwrap(),
                    capture_index_in_machine: MachineCaptureIndex::new(4).unwrap(),
                },
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
                        (PathSegment::Key(key), Some(v)) => v.get(key.as_str()),
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
                        Some(CaptureValue::String(s)) => {
                            assert_eq!(s.resolve(&json), expected.as_str().unwrap());
                        }
                        other => panic!("expected String, got {other:?}"),
                    },
                    (FieldType::Bool, Some(expected)) => {
                        assert_matches!(capture, Some(&CaptureValue::Bool(b)) if b == expected.as_bool().unwrap());
                    }
                    (FieldType::Object, Some(expected)) => match capture {
                        Some(CaptureValue::Object(range)) => {
                            let reparsed: serde_json::Value =
                                serde_json::from_str(&json[range_u32_to_usize(*range)]).unwrap();
                            assert_eq!(&reparsed, expected);
                        }
                        other => panic!("expected Object, got {other:?}"),
                    },
                    (FieldType::Number, None) => {
                        // AnyIndex path: just require that a number was captured.
                        assert!(matches!(capture, Some(CaptureValue::Number(_))));
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

    // ---- exhaustive containers: matching behavior ----

    #[test]
    fn exhaustive_object() {
        let fields = [
            xfield(vec![key("cfg")], FieldType::Object, false),
            field(vec![key("cfg"), key("a")], FieldType::Number, None, false),
            field(vec![key("cfg"), key("b")], FieldType::String, None, false),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(
            run(&machine, r#"{"cfg":{"a":1,"b":"x"}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        // Member order does not matter.
        assert!(
            run(&machine, r#"{"cfg":{"b":"x","a":1}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        // An uncovered key leaves the exhaustive field unsatisfied.
        assert!(
            !run(&machine, r#"{"cfg":{"a":1,"b":"x","c":null}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
    }

    #[test]
    fn exhaustive_object_with_no_children_matches_only_empty() {
        let fields = [xfield(vec![key("cfg")], FieldType::Object, false)];
        let (machine, _) = compile(&[&fields]);
        assert!(run(&machine, r#"{"cfg":{}}"#).unwrap().result.did_match(0));
        assert!(
            !run(&machine, r#"{"cfg":{"k":1}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
    }

    #[test]
    fn exhaustive_object_deep_coverage() {
        // "a" is covered because a same-set field path passes through it, even
        // though no field names "a" itself; sibling keys deeper down are only
        // constrained by their own (non-exhaustive) containers.
        let fields = [
            xfield(vec![key("cfg")], FieldType::Object, false),
            field(
                vec![key("cfg"), key("a"), key("b")],
                FieldType::Number,
                None,
                false,
            ),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(
            run(&machine, r#"{"cfg":{"a":{"b":1}}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        assert!(
            run(&machine, r#"{"cfg":{"a":{"b":1,"z":9}}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        assert!(
            !run(&machine, r#"{"cfg":{"a":{"b":1},"z":9}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
    }

    #[test]
    fn exhaustive_array() {
        let fields = [
            xfield(vec![key("arr")], FieldType::Array, false),
            field(vec![key("arr"), Index(0)], FieldType::Number, None, false),
            field(vec![key("arr"), Index(1)], FieldType::String, None, false),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(
            run(&machine, r#"{"arr":[1,"x"]}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        // An element beyond the covered indices violates exhaustiveness.
        assert!(
            !run(&machine, r#"{"arr":[1,"x",true]}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        // Shorter arrays leave the fixed-index fields unsatisfied instead.
        assert!(!run(&machine, r#"{"arr":[1]}"#).unwrap().result.did_match(0));
        assert!(!run(&machine, r#"{"arr":[]}"#).unwrap().result.did_match(0));
    }

    #[test]
    fn exhaustive_array_hole_never_matches_past_it() {
        // Covered indices {0, 2} with nothing at 1: any array long enough to
        // reach index 2 necessarily contains uncovered index 1.
        let fields = [
            xfield(vec![key("arr")], FieldType::Array, false),
            field(vec![key("arr"), Index(0)], FieldType::Number, None, false),
            field(vec![key("arr"), Index(2)], FieldType::Number, None, false),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(
            !run(&machine, r#"{"arr":[1,2,3]}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        assert!(!run(&machine, r#"{"arr":[1]}"#).unwrap().result.did_match(0));
    }

    #[test]
    fn exhaustive_any_trivial_for_non_containers() {
        let fields = [xfield(vec![key("v")], FieldType::Any, false)];
        let (machine, _) = compile(&[&fields]);
        for good in [
            r#"{"v":5}"#,
            r#"{"v":"s"}"#,
            r#"{"v":true}"#,
            r#"{"v":null}"#,
            r#"{"v":{}}"#,
            r#"{"v":[]}"#,
        ] {
            assert!(run(&machine, good).unwrap().result.did_match(0), "{good}");
        }
        for bad in [r#"{"v":{"k":1}}"#, r#"{"v":[1]}"#] {
            assert!(!run(&machine, bad).unwrap().result.did_match(0), "{bad}");
        }
    }

    #[test]
    fn exhaustive_root() {
        let fields = [
            xfield(vec![], FieldType::Object, false),
            field(vec![key("a")], FieldType::Number, None, false),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(run(&machine, r#"{"a":1}"#).unwrap().result.did_match(0));
        assert!(
            !run(&machine, r#"{"a":1,"b":2}"#)
                .unwrap()
                .result
                .did_match(0)
        );
    }

    #[test]
    fn exhaustive_under_any_index_resets_per_element() {
        // The first element violates exhaustiveness; the second satisfies it,
        // so the violation must not leak between elements.
        let fields = [
            xfield(vec![key("list"), AnyIndex], FieldType::Object, false),
            field(
                vec![key("list"), AnyIndex, key("id")],
                FieldType::Number,
                None,
                false,
            ),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(
            run(&machine, r#"{"list":[{"id":1,"extra":2},{"id":3}]}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        assert!(
            !run(&machine, r#"{"list":[{"id":1,"extra":2}]}"#)
                .unwrap()
                .result
                .did_match(0)
        );
    }

    #[test]
    fn exhaustive_coverage_is_per_set() {
        // Set 1's cfg.b does not cover "b" for set 0's exhaustive cfg.
        let set0 = [
            xfield(vec![key("cfg")], FieldType::Object, false),
            field(vec![key("cfg"), key("a")], FieldType::Number, None, false),
        ];
        let set1 = [field(
            vec![key("cfg"), key("b")],
            FieldType::Number,
            None,
            false,
        )];
        let (machine, _) = compile(&[&set0, &set1]);
        let state = run(&machine, r#"{"cfg":{"a":1,"b":2}}"#).unwrap();
        assert!(!state.result.did_match(0));
        assert!(state.result.did_match(1));
        let state = run(&machine, r#"{"cfg":{"a":1}}"#).unwrap();
        assert!(state.result.did_match(0));
        assert!(!state.result.did_match(1));
    }

    #[test]
    fn exhaustive_cross_set_any_index_never_covers() {
        // Another set's AnyIndex may walk an element, but it does not cover
        // the index for this set's exhaustive array.
        let set0 = [
            xfield(vec![key("arr")], FieldType::Array, false),
            field(vec![key("arr"), Index(0)], FieldType::Any, None, false),
        ];
        let set1 = [field(
            vec![key("arr"), AnyIndex],
            FieldType::String,
            None,
            false,
        )];
        let (machine, _) = compile(&[&set0, &set1]);
        let state = run(&machine, r#"{"arr":["x"]}"#).unwrap();
        assert!(state.result.did_match(0));
        assert!(state.result.did_match(1));
        let state = run(&machine, r#"{"arr":["x","y"]}"#).unwrap();
        assert!(!state.result.did_match(0));
        assert!(state.result.did_match(1));
    }

    #[test]
    fn exhaustive_capture_gated_by_violation() {
        let fields = [
            xfield(vec![key("cfg")], FieldType::Object, true),
            field(vec![key("cfg"), key("a")], FieldType::Number, None, false),
        ];
        let (machine, captures) = compile(&[&fields]);
        let input = r#"{"cfg":{"a":1}}"#;
        let state = run(&machine, input).unwrap();
        assert!(state.result.did_match(0));
        match state.result.capture(captures[&(0, 0, None)]) {
            Some(CaptureValue::Object(range)) => {
                assert_eq!(&input[range_u32_to_usize(*range)], r#"{"a":1}"#)
            }
            other => panic!("expected Object, got {other:?}"),
        }
        // A violated exhaustive container is skipped entirely: no capture.
        let state = run(&machine, r#"{"cfg":{"a":1,"b":2}}"#).unwrap();
        assert!(!state.result.did_match(0));
        assert_matches!(state.result.capture(captures[&(0, 0, None)]), None);
    }

    #[test]
    fn exhaustive_duplicate_members() {
        let fields = [
            xfield(vec![key("cfg")], FieldType::Object, false),
            field(vec![key("cfg"), key("a")], FieldType::Number, None, false),
        ];
        let (machine, _) = compile(&[&fields]);
        // Duplicate covered keys are each covered.
        assert!(
            run(&machine, r#"{"cfg":{"a":1,"a":2}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        // A later duplicate of the container can satisfy the field after an
        // earlier violating instance, matching first-satisfying semantics.
        assert!(
            run(&machine, r#"{"cfg":{"z":1},"cfg":{"a":1}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
    }

    #[test]
    fn exhaustive_wrong_container_kind_unsatisfied() {
        let obj = [xfield(vec![key("cfg")], FieldType::Object, false)];
        let (machine, _) = compile(&[&obj]);
        assert!(!run(&machine, r#"{"cfg":[]}"#).unwrap().result.did_match(0));
        assert!(!run(&machine, r#"{"cfg":1}"#).unwrap().result.did_match(0));
        let arr = [xfield(vec![key("cfg")], FieldType::Array, false)];
        let (machine, _) = compile(&[&arr]);
        assert!(!run(&machine, r#"{"cfg":{}}"#).unwrap().result.did_match(0));
    }

    #[test]
    fn exhaustive_tolerates_whitespace() {
        let fields = [
            xfield(vec![key("cfg")], FieldType::Object, false),
            field(vec![key("cfg"), key("a")], FieldType::Number, None, false),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(
            run(&machine, "{ \"cfg\" : {\n\t\"a\" : 1\r\n} }")
                .unwrap()
                .result
                .did_match(0)
        );
    }

    #[test]
    fn exhaustive_escaped_key_coverage() {
        // The escaped spelling of a covered key still counts as covered.
        let fields = [
            xfield(vec![key("cfg")], FieldType::Object, false),
            field(vec![key("cfg"), key("A")], FieldType::Number, None, false),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(
            run(&machine, r#"{"cfg":{"A":1}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
    }

    #[test]
    fn exhaustive_at_fixed_index_with_merged_wildcard() {
        // a[*].x's subtree is merged into a[0]'s node at compile time; the
        // merged same-set actions must count as coverage for a[0]'s
        // exhaustive check.
        let fields = [
            xfield(vec![key("a"), Index(0)], FieldType::Object, false),
            field(
                vec![key("a"), AnyIndex, key("x")],
                FieldType::Number,
                None,
                false,
            ),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(
            run(&machine, r#"{"a":[{"x":1}]}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        assert!(
            run(&machine, r#"{"a":[{"x":1},{"y":5}]}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        assert!(
            !run(&machine, r#"{"a":[{"x":1,"z":2}]}"#)
                .unwrap()
                .result
                .did_match(0)
        );
    }

    #[test]
    fn exhaustive_nested() {
        let fields = [
            xfield(vec![key("outer")], FieldType::Object, false),
            xfield(vec![key("outer"), key("inner")], FieldType::Object, false),
            field(
                vec![key("outer"), key("inner"), key("a")],
                FieldType::Number,
                None,
                false,
            ),
        ];
        let (machine, _) = compile(&[&fields]);
        assert!(
            run(&machine, r#"{"outer":{"inner":{"a":1}}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        // Violation at either level breaks the set.
        assert!(
            !run(&machine, r#"{"outer":{"inner":{"a":1},"z":2}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
        assert!(
            !run(&machine, r#"{"outer":{"inner":{"a":1,"z":2}}}"#)
                .unwrap()
                .result
                .did_match(0)
        );
    }

    // ---- pattern validation ----

    #[test]
    fn validate_literal_json() {
        for good in [
            "[1,2,3]",
            r#"{"a":1}"#,
            r#""s""#,
            "null",
            "-0.5e2",
            "[1, 2]",
        ] {
            let fields = [field(
                vec![key("l")],
                FieldType::Literal(good.into()),
                None,
                false,
            )];
            assert!(
                MatchMachine::compile([Pattern { fields: &fields }].into_iter(), |_| {}).is_ok(),
                "should compile: {good}"
            );
        }
        for bad in [
            "nope",
            "01",
            "1 2",
            " 1",
            "1 ",
            "1.",
            "",
            "tru",
            r#"{"a":}"#,
        ] {
            let fields = [field(
                vec![key("l")],
                FieldType::Literal(bad.into()),
                None,
                false,
            )];
            assert_matches!(
                compile_err(&[&fields]),
                CompileError::InvalidLiteral {
                    set_index: 0,
                    field_index: 0
                },
                "should be invalid: {bad:?}"
            );
        }
    }

    #[test]
    fn validate_container_type_mismatch() {
        let cases: Vec<[FieldPattern<'static>; 2]> = vec![
            [
                field(vec![key("foo")], FieldType::Object, None, false),
                field(vec![key("foo"), Index(0)], FieldType::Any, None, false),
            ],
            [
                field(vec![key("foo")], FieldType::Array, None, false),
                field(vec![key("foo"), key("k")], FieldType::Any, None, false),
            ],
            [
                field(vec![key("foo")], FieldType::Number, None, false),
                field(vec![key("foo"), key("baz")], FieldType::Any, None, false),
            ],
            [
                field(
                    vec![key("foo")],
                    FieldType::Literal("{}".into()),
                    None,
                    false,
                ),
                field(vec![key("foo"), key("k")], FieldType::Any, None, false),
            ],
        ];
        for fields in &cases {
            assert_matches!(
                compile_err(&[fields]),
                CompileError::ContainerTypeMismatch {
                    set_index: 0,
                    container_field_index: 0,
                    field_index: 1
                }
            );
        }
        // Field order does not matter.
        let swapped = [
            field(vec![key("foo"), Index(0)], FieldType::Any, None, false),
            field(vec![key("foo")], FieldType::Object, None, false),
        ];
        assert_matches!(
            compile_err(&[&swapped]),
            CompileError::ContainerTypeMismatch {
                set_index: 0,
                container_field_index: 1,
                field_index: 0
            }
        );
        // Compatible combinations compile.
        let good = [
            field(vec![key("o")], FieldType::Object, None, false),
            field(vec![key("o"), key("k")], FieldType::Any, None, false),
            field(vec![key("a")], FieldType::Array, None, false),
            field(vec![key("a"), Index(0)], FieldType::Any, None, false),
            field(vec![key("a"), AnyIndex], FieldType::Any, None, false),
            field(vec![key("any")], FieldType::Any, None, false),
            field(vec![key("any"), key("k")], FieldType::Any, None, false),
        ];
        assert!(MatchMachine::compile([Pattern { fields: &good }].into_iter(), |_| {}).is_ok());
        // Different sets may disagree about a path's type.
        let set0 = [field(vec![key("foo")], FieldType::Object, None, false)];
        let set1 = [field(
            vec![key("foo"), Index(0)],
            FieldType::Any,
            None,
            false,
        )];
        assert!(
            MatchMachine::compile(
                [Pattern { fields: &set0 }, Pattern { fields: &set1 }].into_iter(),
                |_| {}
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_conflicting_container_kinds() {
        // No declared type for "foo", but its two descents disagree.
        let fields = [
            field(vec![key("foo"), key("a")], FieldType::Any, None, false),
            field(vec![key("foo"), Index(0)], FieldType::Any, None, false),
        ];
        assert_matches!(
            compile_err(&[&fields]),
            CompileError::ConflictingContainerKinds {
                set_index: 0,
                field_index_a: 0,
                field_index_b: 1
            }
        );
        // Deeper shared prefix.
        let fields = [
            field(
                vec![key("x"), key("y"), key("a")],
                FieldType::Any,
                None,
                false,
            ),
            field(
                vec![key("x"), key("y"), AnyIndex],
                FieldType::Any,
                None,
                false,
            ),
        ];
        assert_matches!(
            compile_err(&[&fields]),
            CompileError::ConflictingContainerKinds { .. }
        );
        // Index and AnyIndex agree on the kind; sibling keys agree trivially.
        let good = [
            field(vec![key("a"), Index(0)], FieldType::Any, None, false),
            field(vec![key("a"), AnyIndex], FieldType::Any, None, false),
            field(vec![key("o"), key("x")], FieldType::Any, None, false),
            field(vec![key("o"), key("y")], FieldType::Any, None, false),
        ];
        assert!(MatchMachine::compile([Pattern { fields: &good }].into_iter(), |_| {}).is_ok());
    }

    #[test]
    fn validate_any_index_under_exhaustive() {
        // Directly below the exhaustive array.
        let fields = [
            xfield(vec![key("arr")], FieldType::Array, false),
            field(vec![key("arr"), AnyIndex], FieldType::Any, None, false),
        ];
        assert_matches!(
            compile_err(&[&fields]),
            CompileError::AnyIndexUnderExhaustive {
                set_index: 0,
                container_field_index: 0,
                field_index: 1
            }
        );
        // Anywhere deeper below the exhaustive container is also rejected.
        let fields = [
            xfield(vec![key("cfg")], FieldType::Object, false),
            field(
                vec![key("cfg"), key("list"), AnyIndex],
                FieldType::Any,
                None,
                false,
            ),
        ];
        assert_matches!(
            compile_err(&[&fields]),
            CompileError::AnyIndexUnderExhaustive { .. }
        );
        // AnyIndex inside the exhaustive field's own prefix is fine.
        let good = [
            xfield(vec![key("list"), AnyIndex], FieldType::Object, false),
            field(
                vec![key("list"), AnyIndex, key("id")],
                FieldType::Number,
                None,
                false,
            ),
        ];
        assert!(MatchMachine::compile([Pattern { fields: &good }].into_iter(), |_| {}).is_ok());
        // Another set descending with AnyIndex is allowed (coverage is
        // per-set; see exhaustive_cross_set_any_index_never_covers).
        let set0 = [xfield(vec![key("arr")], FieldType::Array, false)];
        let set1 = [field(
            vec![key("arr"), AnyIndex],
            FieldType::Any,
            None,
            false,
        )];
        assert!(
            MatchMachine::compile(
                [Pattern { fields: &set0 }, Pattern { fields: &set1 }].into_iter(),
                |_| {}
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_exhaustive_unsupported_type() {
        for ty in [
            FieldType::String,
            FieldType::Number,
            FieldType::Bool,
            FieldType::Null,
            FieldType::Literal("1".into()),
        ] {
            let fields = [xfield(vec![key("v")], ty.clone(), false)];
            assert_matches!(
                compile_err(&[&fields]),
                CompileError::ExhaustiveUnsupportedType {
                    set_index: 0,
                    field_index: 0
                },
                "should reject exhaustive {ty:?}"
            );
        }
        for ty in [FieldType::Object, FieldType::Array, FieldType::Any] {
            let fields = [xfield(vec![key("v")], ty, false)];
            assert!(
                MatchMachine::compile([Pattern { fields: &fields }].into_iter(), |_| {}).is_ok()
            );
        }
    }

    #[test]
    fn print_generater_json() {
        let mut rng = StdRng::seed_from_u64(0xB00B5);
        let json = generate_test_json(&test_fields(), 1.0, 64.0, &mut rng);
        println!("{}", json);
    }
}
