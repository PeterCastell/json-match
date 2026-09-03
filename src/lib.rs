#![deny(unused_must_use)]
#![forbid(unsafe_code)]

use std::range::Range;
use std::sync::LazyLock;
use std::{fmt, num::NonZero};

use bitvec::{bitbox, boxed::BitBox, order::Lsb0};
use compact_str::CompactString;
use integer_encoding::VarInt;
use regex::{CaptureLocations, Regex};
use soa_rs::{Soa, Soars};

#[cfg(any(test, feature = "benchmarking"))]
pub mod testing;

#[derive(Debug, Clone, Copy)]
pub struct Pattern<'a> {
    pub fields: &'a [FieldPattern<'a>],
}

#[derive(Debug, Clone)]
pub struct FieldPattern<'a> {
    pub path: &'a [PathSegment],
    pub r#type: FieldType,
    /// Regex the value must satisfy for the field to count as present. Runs
    /// unanchored (a substring match anywhere in the value); anchor with
    /// `^`/`$` to require a full-value match. For strings the haystack is the
    /// unescaped content; for every other type it is the raw JSON text.
    pub predicate: Option<Regex>,
    pub capture: bool,
    /// Marks this container as exhaustive: every member of the matched value
    /// (object key / array index) must lie on the path of another field of the
    /// same set that descends through this container, or this field is left
    /// unsatisfied. Only [`FieldType::Object`], [`FieldType::Array`], and
    /// [`FieldType::Any`] support it (for `Any`, non-container values are
    /// trivially exhaustive), and same-set fields may not descend an
    /// exhaustive container with [`PathSegment::AnyIndex`].
    pub exhaustive: bool,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone)]
pub enum PathSegment {
    Key(CompactString),
    /// Fixed array index.
    Index(u16),
    /// First array element whose continuation satisfies this field.
    /// Independent per field: two fields sharing an AnyIndex prefix may be
    /// satisfied by different elements.
    AnyIndex,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone)]
pub enum FieldType {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
    /// Raw byte-for-byte comparison against the value's text, whitespace-sensitive.
    /// Example: `Literal("[1,2,3]".into())`
    Literal(CompactString),
    /// Match any value, type is returned through CaptureValue.
    Any,
}

fn usize_to_u24(x: usize) -> Option<(u16, u8)> {
    if x > 0xFFFFFF {
        None
    } else {
        Some((x as u16, (x >> 16) as u8))
    }
}

fn u24_to_usize(lsb: u16, msb: u8) -> usize {
    ((msb as usize) << 16) | (lsb as usize)
}

#[derive(Debug, Clone, Copy)]
pub enum MachineFieldType {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
    /// Raw byte-for-byte comparison against the value's text, whitespace-sensitive.
    /// Example: `Literal("[1,2,3]".into())`
    Literal {
        len: u32,
        lsb: u16,
        msb: u8,
    },
    /// Match any value, type is returned through CaptureValue.
    Any,
}

#[derive(Debug, Clone)]
pub enum CaptureValue {
    PredicateCapture(UnescapedString),
    Object(Range<u32>),
    Array(Range<u32>),
    String(UnescapedString),
    /// Parsed as f64: integers with magnitude above 2^53 lose precision.
    /// Literals outside the finite f64 range fail the match with
    /// [`MatchError::NumberOutOfRange`] instead of capturing ±inf.
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone)]
pub enum UnescapedString {
    /// The value contained no escape sequences; the range indexes the original input.
    Borrowed(Range<u32>),
    Owned(CompactString),
}

impl UnescapedString {
    pub fn resolve<'s>(&'s self, input: &'s str) -> &'s str {
        match self {
            UnescapedString::Borrowed(range) => &input[range_u32_to_usize(*range)],
            UnescapedString::Owned(string) => string,
        }
    }
}

#[inline]
pub fn range_u32_to_usize(range: Range<u32>) -> Range<usize> {
    Range {
        start: range.start as usize,
        end: range.end as usize,
    }
}

#[inline]
fn range_u32(start: u32, end: u32) -> Range<u32> {
    Range {
        start: start,
        end: end,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineCaptureIndex(NonZero<u32>);

impl MachineCaptureIndex {
    pub const fn new(index: u32) -> Option<Self> {
        match NonZero::new(index + 1) {
            Some(i) => Some(Self(i)),
            None => None,
        }
    }

    pub fn get(self) -> u32 {
        self.0.get() - 1
    }
}

impl fmt::Debug for MachineCaptureIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("MachineCaptureIndex")
            .field(&self.get())
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SetCaptureIndex(NonZero<u32>);

impl SetCaptureIndex {
    pub const fn new(index: u32) -> Option<Self> {
        match NonZero::new(index + 1) {
            Some(i) => Some(Self(i)),
            None => None,
        }
    }

    pub fn get(self) -> u32 {
        self.0.get() - 1
    }
}

impl fmt::Debug for SetCaptureIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SetCaptureIndex").field(&self.get()).finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureCallbackArgs<'a> {
    pub match_set_index: u32,
    pub field_index: u32,
    pub predicate_capture_name: Option<&'a str>,
    pub capture_index_in_set: SetCaptureIndex,
    pub capture_index_in_machine: MachineCaptureIndex,
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    /// A [`FieldType::Literal`] whose text is not exactly one valid JSON
    /// value. Leading and trailing whitespace also count as invalid: literal
    /// comparison is byte-for-byte against a value span, which never includes
    /// surrounding whitespace, so a padded literal could never match.
    #[error("literal of field {field_index} in set {set_index} is not valid JSON")]
    InvalidLiteral { set_index: u32, field_index: u32 },
    #[error(
        "literal of field {field_index} in set {set_index} is greater than 2^32 bytes in length"
    )]
    LiteralTooLong { set_index: u32, field_index: u32 },
    /// A field's path descends another field whose declared type contradicts
    /// the descent: an Object descended with an index, an Array descended with
    /// a key, or a non-container type descended at all. `Any` accepts either
    /// kind of descent.
    #[error(
        "field {field_index} in set {set_index} descends field {container_field_index}, whose declared type does not allow it"
    )]
    ContainerTypeMismatch {
        set_index: u32,
        container_field_index: u32,
        field_index: u32,
    },
    /// Two fields of the same set descend the same path position as different
    /// container kinds (one as an object key, the other as an array index), so
    /// no value could ever satisfy both.
    #[error(
        "fields {field_index_a} and {field_index_b} in set {set_index} imply different container types for the same path"
    )]
    ConflictingContainerKinds {
        set_index: u32,
        field_index_a: u32,
        field_index_b: u32,
    },
    /// A field descends an exhaustive container of the same set using
    /// [`PathSegment::AnyIndex`] anywhere below it, which would make the
    /// exhaustive coverage of array indices unknowable.
    #[error(
        "field {field_index} in set {set_index} descends exhaustive field {container_field_index} with AnyIndex"
    )]
    AnyIndexUnderExhaustive {
        set_index: u32,
        container_field_index: u32,
        field_index: u32,
    },
    /// `exhaustive` set on a field whose type has no members to enumerate;
    /// only Object, Array, and Any support it.
    #[error(
        "field {field_index} in set {set_index} is exhaustive but its type does not support it"
    )]
    ExhaustiveUnsupportedType { set_index: u32, field_index: u32 },
    #[error("the number of captures exceeds 2^32 - 1, which is an awful lot")]
    TooManyCaptures,
    #[error("the number of literals exceeds 2^24, which is an awful lot")]
    TooManyLiterals,
}

#[derive(Debug, thiserror::Error)]
pub enum MatchError {
    #[error("unexpected byte 0x{byte:02x} at offset {pos}")]
    UnexpectedByte { pos: u32, byte: u8 },
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("trailing data at offset {pos}")]
    TrailingData { pos: u32 },
    #[error("invalid number at offset {pos}")]
    InvalidNumber { pos: u32 },
    /// A captured number literal is outside the finite f64 range (e.g.
    /// `1e999`). Only captured numbers are parsed; uncaptured ones are
    /// validated syntactically without a range check.
    #[error("number out of f64 range at offset {pos}")]
    NumberOutOfRange { pos: u32 },
    #[error("input length exceeds u32::MAX bytes")]
    InputTooLong,
    #[error("invalid string escape at offset {pos}")]
    InvalidEscape { pos: u32 },
    #[error("nesting depth limit exceeded at offset {pos}")]
    DepthLimitExceeded { pos: u32 },
}
// NOTE: outlining error construction into #[cold] #[inline(never)] helper fns
// was benchmarked and lost to inline construction by 1-5% at every bloat
// level: the enum is small enough that a call's register clobbers in the hot
// loops cost more than the never-taken inline writes.

#[derive(Debug, Clone)]
struct Action {
    set_index: u32,
    /// Global field id across all sets; indexes MachineState.satisfied.
    field_bit: u32,
    type_check: MachineFieldType,
    predicate: Option<Regex>,
    /// Index into MachineState.capture_locs; u32::MAX when predicate is None.
    predicate_loc: u32,
    /// (regex group index, machine capture index) for named groups not starting with '_'.
    predicate_groups: Box<[(u32, NonZero<u32>)]>,
    value_capture: Option<NonZero<u32>>,
    /// Coverage for an exhaustive container field; None for ordinary fields.
    /// Placeholder (empty) until compile's coverage pass fills it in.
    exhaustive: Option<ExhaustiveCoverage>,
}

/// One bit per machine node: whether that node's subtree contains any action
/// of the exhaustive field's set. A container member resolving to no child or
/// to an uncovered child violates the exhaustive field.
#[derive(Debug, Clone, Default)]
struct ExhaustiveCoverage {
    covered_nodes: BitBox,
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
struct NodeId(u32);

impl NodeId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, Soars, soa_rs::SoaClone)]
#[soa_derive(Debug)]
struct IndexChild {
    index: u16,
    child: NodeId,
}

#[derive(Debug)]
struct Node {
    key_children: Box<[(CompactString, NodeId)]>,
    /// Sorted by index. When a node also has a wildcard child, each fixed-index
    /// subtree already contains a merged copy of the wildcard subtree, so every
    /// array element resolves to at most one node.
    index_children: Soa<IndexChild>,
    any_index_child: Option<NodeId>,
    actions: Range<u32>,
    /// Whether any action of this node is exhaustive: containers are then
    /// always walked (never skipped via validate_value) so coverage of their
    /// members can be checked.
    has_exhaustive: bool,
}

#[derive(Debug)]
pub struct MatchMachine {
    captures_length: u32,
    fields_length: u32,
    predicates_length: u32,
    set_required_counts: Box<[u32]>,
    nodes: Box<[Node]>,
    actions: Box<[Action]>,
    literal_pool: Box<[u8]>,
}

#[derive(Debug)]
pub struct MachineResult {
    capture_values: Box<[Option<CaptureValue>]>,
    match_results: BitBox, // whether each set by index matched
}

impl MachineResult {
    pub fn did_match(&self, set_index: u32) -> bool {
        self.match_results[set_index as usize]
    }

    pub fn match_results(&self) -> &BitBox {
        &self.match_results
    }

    pub fn matches(&self) -> impl Iterator<Item = u32> {
        self.match_results.iter_ones().map(|i| i as u32)
    }

    pub fn capture(&self, machine_capture_index: MachineCaptureIndex) -> Option<&CaptureValue> {
        self.capture_values[machine_capture_index.get() as usize].as_ref()
    }

    pub fn captures(&self) -> &[Option<CaptureValue>] {
        &self.capture_values
    }
}

pub const DEFAULT_DEPTH_LIMIT: usize = 128;

#[derive(Debug)]
pub struct MachineState {
    pub result: MachineResult,
    satisfied: BitBox,
    set_counts: Box<[u32]>,
    capture_locs: Box<[CaptureLocations]>,
    unescape_buf: String,
    /// One bit per open container while validating skipped regions
    /// (1 = object, 0 = array); its length is the nesting depth limit.
    bracket_stack: BitBox,
    /// One bit per machine action: whether the container instance currently
    /// being walked violated that exhaustive action. Reset by the walk of each
    /// container instance and read by run_actions right after it, so no
    /// per-match reset is needed.
    exhaustive_violated: BitBox,
}

impl MachineState {
    /// Maximum container nesting depth accepted while validating regions the
    /// pattern does not descend into. Nesting along matched paths is bounded
    /// by the compiled pattern instead and does not count against this limit.
    pub fn depth_limit(&self) -> usize {
        self.bracket_stack.len()
    }
    pub fn set_depth_limit(&mut self, limit: usize) {
        self.bracket_stack = bitbox![usize, Lsb0; 0; limit];
    }
}

#[derive(Default)]
struct NodeBuild {
    key_children: Vec<(CompactString, NodeId)>,
    index_children: Soa<IndexChild>,
    any_index_child: Option<NodeId>,
    actions: Vec<Action>,
}

fn push_node(nodes: &mut Vec<NodeBuild>) -> NodeId {
    let index = nodes.len() as u32;
    nodes.push(NodeBuild::default());
    NodeId(index)
}

fn get_or_create_child(
    nodes: &mut Vec<NodeBuild>,
    parent: NodeId,
    segment: &PathSegment,
) -> NodeId {
    match *segment {
        PathSegment::Key(ref key) => {
            let existing = nodes[parent.index()]
                .key_children
                .iter()
                .find(|(k, _)| k == key)
                .map(|&(_, child)| child);
            existing.unwrap_or_else(|| {
                let child = push_node(nodes);
                nodes[parent.index()]
                    .key_children
                    .push((key.clone(), child));
                child
            })
        }
        PathSegment::Index(index) => {
            let existing = nodes[parent.index()]
                .index_children
                .iter()
                .find(|&IndexChildRef { index: &i, .. }| i == index)
                .map(|IndexChildRef { child, .. }| *child);
            existing.unwrap_or_else(|| {
                let child = push_node(nodes);
                nodes[parent.index()]
                    .index_children
                    .push(IndexChild { index, child });
                child
            })
        }
        PathSegment::AnyIndex => match nodes[parent.index()].any_index_child {
            Some(child) => child,
            None => {
                let child = push_node(nodes);
                nodes[parent.index()].any_index_child = Some(child);
                child
            }
        },
    }
}

/// Union the src subtree into the dst subtree, cloning actions. Duplicated
/// actions share their field_bit, so the satisfied bitset dedupes them at runtime.
fn merge_subtree(nodes: &mut Vec<NodeBuild>, dst: NodeId, src: NodeId) {
    let src_actions = nodes[src.index()].actions.clone();
    nodes[dst.index()].actions.extend(src_actions);
    let src_keys = nodes[src.index()].key_children.clone();
    for (key, src_child) in src_keys {
        let dst_child = get_or_create_child(nodes, dst, &PathSegment::Key(key));
        merge_subtree(nodes, dst_child, src_child);
    }
    let src_indices = nodes[src.index()].index_children.clone();
    for IndexChildRef {
        index,
        child: &src_child,
    } in &src_indices
    {
        let dst_child = get_or_create_child(nodes, dst, &PathSegment::Index(*index));
        merge_subtree(nodes, dst_child, src_child);
    }
    if let Some(src_any) = nodes[src.index()].any_index_child {
        let dst_any = get_or_create_child(nodes, dst, &PathSegment::AnyIndex);
        merge_subtree(nodes, dst_any, src_any);
    }
}

/// Wherever a node has both fixed-index children and a wildcard child, merge the
/// wildcard subtree into each fixed subtree so array elements resolve to one node.
/// Compile-time cost: every fixed-index sibling gets its own copy of the wildcard
/// subtree, so patterns stacking wildcards under many fixed indices at several
/// levels can grow multiplicatively. Match-time cost is unaffected.
fn merge_wildcards(nodes: &mut Vec<NodeBuild>, node: NodeId) {
    if let Some(any) = nodes[node.index()].any_index_child {
        let fixed: Vec<NodeId> = nodes[node.index()]
            .index_children
            .child()
            .iter()
            .copied()
            .collect();
        for child in fixed {
            merge_subtree(nodes, child, any);
        }
    }
    let mut children: Vec<NodeId> = nodes[node.index()]
        .key_children
        .iter()
        .map(|&(_, c)| c)
        .collect();
    children.extend(nodes[node.index()].index_children.child().iter().copied());
    children.extend(nodes[node.index()].any_index_child);
    for child in children {
        merge_wildcards(nodes, child);
    }
}

fn segments_equal(a: &PathSegment, b: &PathSegment) -> bool {
    match (a, b) {
        (PathSegment::Key(x), PathSegment::Key(y)) => x == y,
        (PathSegment::Index(x), PathSegment::Index(y)) => x == y,
        (PathSegment::AnyIndex, PathSegment::AnyIndex) => true,
        _ => false,
    }
}

/// Whether text is exactly one valid JSON value, with no surrounding
/// whitespace. Reuses the matcher's validator on an empty machine.
fn literal_is_valid_json(text: &str) -> bool {
    if text.len() > u32::MAX as usize {
        return false;
    }
    static MACHINE: LazyLock<MatchMachine> = LazyLock::new(|| MatchMachine {
        captures_length: 0,
        fields_length: 0,
        predicates_length: 0,
        set_required_counts: Box::new([]),
        nodes: Box::new([]),
        actions: Box::new([]),
        literal_pool: Box::new([]),
    });
    let mut state = MACHINE.allocate_state();
    // A value of len bytes nests at most len/2 levels; the literal itself
    // must never fail on depth.
    state.set_depth_limit(text.len() / 2 + 1);
    let matcher = Matcher {
        machine: &MACHINE,
        input: text,
        bytes: text.as_bytes(),
    };
    matches!(matcher.validate_value(0, &mut state), Ok(end) if end as usize == text.len())
}

/// Per-set structural validation; see [`CompileError`] for the rules.
fn validate_pattern(set_index: u32, fields: &[FieldPattern<'_>]) -> Result<(), CompileError> {
    for (field_index, field) in fields.iter().enumerate() {
        let field_index = field_index as u32;
        if field.exhaustive
            && !matches!(
                field.r#type,
                FieldType::Object | FieldType::Array | FieldType::Any
            )
        {
            return Err(CompileError::ExhaustiveUnsupportedType {
                set_index,
                field_index,
            });
        }
    }
    for (a_index, a) in fields.iter().enumerate() {
        for (b_index, b) in fields.iter().enumerate() {
            if a_index == b_index {
                continue;
            }
            let common = a
                .path
                .iter()
                .zip(b.path)
                .take_while(|(x, y)| segments_equal(x, y))
                .count();
            if common == a.path.len() && common < b.path.len() {
                // b descends a.
                let compatible = match (&a.r#type, &b.path[common]) {
                    (FieldType::Any, _) => true,
                    (FieldType::Object, PathSegment::Key(_)) => true,
                    (FieldType::Array, PathSegment::Index(_) | PathSegment::AnyIndex) => true,
                    _ => false,
                };
                if !compatible {
                    return Err(CompileError::ContainerTypeMismatch {
                        set_index,
                        container_field_index: a_index as u32,
                        field_index: b_index as u32,
                    });
                }
                if a.exhaustive
                    && b.path[common..]
                        .iter()
                        .any(|segment| matches!(segment, PathSegment::AnyIndex))
                {
                    return Err(CompileError::AnyIndexUnderExhaustive {
                        set_index,
                        container_field_index: a_index as u32,
                        field_index: b_index as u32,
                    });
                }
            } else if a_index < b_index && common < a.path.len() && common < b.path.len() {
                // Paths diverge: the diverging segments must agree on the
                // container kind they expect at the shared prefix.
                let a_is_key = matches!(a.path[common], PathSegment::Key(_));
                let b_is_key = matches!(b.path[common], PathSegment::Key(_));
                if a_is_key != b_is_key {
                    return Err(CompileError::ConflictingContainerKinds {
                        set_index,
                        field_index_a: a_index as u32,
                        field_index_b: b_index as u32,
                    });
                }
            }
        }
    }
    Ok(())
}

impl MatchMachine {
    pub fn num_match_sets(&self) -> u32 {
        self.set_required_counts.len() as u32
    }
    pub fn num_captures(&self) -> u32 {
        self.captures_length
    }

    pub fn allocate_state(&self) -> MachineState {
        let mut locs: Vec<Option<CaptureLocations>> =
            (0..self.predicates_length).map(|_| None).collect();
        for action in &self.actions {
            if let Some(predicate) = &action.predicate {
                let slot = &mut locs[action.predicate_loc as usize];
                if slot.is_none() {
                    *slot = Some(predicate.capture_locations());
                }
            }
        }
        MachineState {
            result: MachineResult {
                capture_values: vec![None; self.captures_length as usize].into_boxed_slice(),
                match_results: bitbox![usize, Lsb0; 0; self.set_required_counts.len()],
            },
            satisfied: bitbox![usize, Lsb0; 0; self.fields_length as usize],
            set_counts: vec![0u32; self.set_required_counts.len()].into_boxed_slice(),
            capture_locs: locs.into_iter().map(|slot| slot.unwrap()).collect(),
            unescape_buf: String::new(),
            bracket_stack: bitbox![usize, Lsb0; 0; DEFAULT_DEPTH_LIMIT],
            exhaustive_violated: bitbox![usize, Lsb0; 0; self.actions.len()],
        }
    }

    pub fn compile<'a>(
        match_sets: impl Iterator<Item = Pattern<'a>>,
        mut capture_index_callback: impl FnMut(CaptureCallbackArgs<'a>),
    ) -> Result<MatchMachine, CompileError> {
        let mut nodes: Vec<NodeBuild> = vec![NodeBuild::default()];
        let mut set_required_counts: Vec<u32> = Vec::new();
        let mut next_machine_capture_index: NonZero<u32> = NonZero::new(1).unwrap();
        let mut next_field_bit: u32 = 0;
        let mut next_predicate_loc: u32 = 0;

        let mut literal_pool: Vec<u8> = Vec::new();

        for (set_index, set) in match_sets.enumerate() {
            validate_pattern(set_index as u32, set.fields)?;
            set_required_counts.push(set.fields.len() as u32);
            let mut next_set_capture_index: NonZero<u32> = NonZero::new(1).unwrap();
            for (field_index, field) in set.fields.iter().enumerate() {
                let mut node: NodeId = NodeId(0);
                for segment in field.path {
                    node = get_or_create_child(&mut nodes, node, segment);
                }

                let value_capture = if field.capture {
                    capture_index_callback(CaptureCallbackArgs {
                        match_set_index: set_index as u32,
                        field_index: field_index as u32,
                        predicate_capture_name: None,
                        capture_index_in_set: SetCaptureIndex(next_set_capture_index),
                        capture_index_in_machine: MachineCaptureIndex(next_machine_capture_index),
                    });
                    let index = next_machine_capture_index;
                    next_machine_capture_index = next_machine_capture_index
                        .checked_add(1)
                        .ok_or_else(|| CompileError::TooManyCaptures)?;
                    next_set_capture_index = next_set_capture_index.checked_add(1).unwrap();
                    Some(index)
                } else {
                    None
                };

                let mut predicate_groups = Vec::new();
                let mut predicate_loc = u32::MAX;
                if let Some(predicate) = &field.predicate {
                    for (group_index, name) in predicate.capture_names().enumerate() {
                        if let Some(name) = name
                            && !name.starts_with('_')
                        {
                            capture_index_callback(CaptureCallbackArgs {
                                match_set_index: set_index as u32,
                                field_index: field_index as u32,
                                predicate_capture_name: Some(name),
                                capture_index_in_set: SetCaptureIndex(next_set_capture_index),
                                capture_index_in_machine: MachineCaptureIndex(
                                    next_machine_capture_index,
                                ),
                            });
                            predicate_groups.push((group_index as u32, next_machine_capture_index));
                            next_machine_capture_index = next_machine_capture_index
                                .checked_add(1)
                                .ok_or_else(|| CompileError::TooManyCaptures)?;
                            next_set_capture_index = next_set_capture_index.checked_add(1).unwrap();
                        }
                    }
                    predicate_loc = next_predicate_loc;
                    next_predicate_loc += 1;
                }

                nodes[node.index()].actions.push(Action {
                    set_index: set_index as u32,
                    field_bit: next_field_bit,
                    type_check: match field.r#type {
                        FieldType::Object => MachineFieldType::Object,
                        FieldType::Array => MachineFieldType::Array,
                        FieldType::String => MachineFieldType::String,
                        FieldType::Number => MachineFieldType::Number,
                        FieldType::Bool => MachineFieldType::Bool,
                        FieldType::Null => MachineFieldType::Null,
                        FieldType::Literal(ref s) => {
                            if !literal_is_valid_json(s) {
                                return Err(CompileError::InvalidLiteral {
                                    set_index: set_index as u32,
                                    field_index: field_index as u32,
                                });
                            }

                            let pool_index = literal_pool.len();
                            let len: u32 =
                                s.len()
                                    .try_into()
                                    .map_err(|_| CompileError::LiteralTooLong {
                                        set_index: set_index as u32,
                                        field_index: field_index as u32,
                                    })?;
                            // let mut len_buf = [0u8; 5];
                            // let len_buf_n = len.encode_var(&mut len_buf);
                            // literal_pool.extend_from_slice(&len_buf[0..len_buf_n]);
                            literal_pool.extend_from_slice(s.as_bytes());
                            let Some((lsb, msb)) = usize_to_u24(pool_index) else {
                                return Err(CompileError::TooManyLiterals);
                            };
                            MachineFieldType::Literal { len, lsb, msb }
                        }
                        FieldType::Any => MachineFieldType::Any,
                    },
                    predicate: field.predicate.clone(),
                    predicate_loc,
                    predicate_groups: predicate_groups.into_boxed_slice(),
                    value_capture,
                    exhaustive: field.exhaustive.then(ExhaustiveCoverage::default),
                });
                next_field_bit += 1;
            }
        }

        merge_wildcards(&mut nodes, NodeId(0));

        let mut actions: Vec<Action> = Vec::new();
        let mut final_nodes: Vec<Node> = Vec::with_capacity(nodes.len());
        for mut build in nodes {
            let start = actions.len() as u32;
            let has_exhaustive = build
                .actions
                .iter()
                .any(|action| action.exhaustive.is_some());
            actions.append(&mut build.actions);
            let mut index_children = build.index_children.into_iter().collect::<Vec<_>>();
            index_children.sort_unstable_by_key(|&IndexChild { index, .. }| index);
            final_nodes.push(Node {
                key_children: build.key_children.into_boxed_slice(),
                index_children: index_children.into_iter().collect(),
                any_index_child: build.any_index_child,
                actions: Range {
                    start,
                    end: actions.len() as u32,
                },
                has_exhaustive,
            });
        }

        // Exhaustive coverage pass: a child is covered for an exhaustive
        // action when the child's subtree contains any action of the same set
        // (the child then lies on that set's field paths). Children always
        // have higher node indices than their parent, so a reverse scan sees
        // every child's set membership before its parent needs it.
        let num_sets = set_required_counts.len();
        let mut node_sets: Vec<BitBox> = vec![bitbox![usize, Lsb0; 0; num_sets]; final_nodes.len()];
        for node_index in (0..final_nodes.len()).rev() {
            let node = &final_nodes[node_index];
            let mut sets = bitbox![usize, Lsb0; 0; num_sets];
            for action in &actions[range_u32_to_usize(node.actions)] {
                sets.set(action.set_index as usize, true);
            }
            let children = node
                .key_children
                .iter()
                .map(|&(_, child)| child)
                .chain(node.index_children.child().iter().copied())
                .chain(node.any_index_child);
            for child in children {
                for set in node_sets[child.index()].iter_ones() {
                    sets.set(set, true);
                }
            }
            node_sets[node_index] = sets;
        }
        for action in actions.iter_mut() {
            let set = action.set_index as usize;
            if let Some(coverage) = &mut action.exhaustive {
                coverage.covered_nodes = node_sets
                    .iter()
                    .map(|sets| sets[set])
                    .collect::<bitvec::vec::BitVec>()
                    .into_boxed_bitslice();
            }
        }

        Ok(MatchMachine {
            captures_length: next_machine_capture_index.get() - 1,
            fields_length: next_field_bit,
            predicates_length: next_predicate_loc,
            set_required_counts: set_required_counts.into_boxed_slice(),
            nodes: final_nodes.into_boxed_slice(),
            actions: actions.into_boxed_slice(),
            literal_pool: literal_pool.into_boxed_slice(),
        })
    }

    /// Inputs are limited to u32::MAX bytes; longer strings are rejected with
    /// [`MatchError::InputTooLong`].
    pub fn match_string(&self, string: &str, state: &mut MachineState) -> Result<(), MatchError> {
        if string.len() > u32::MAX as usize {
            return Err(MatchError::InputTooLong);
        }

        state.result.capture_values.fill_with(|| None);
        state.result.match_results.fill(false);
        state.satisfied.fill(false);
        state.set_counts.fill(0);

        let matcher = Matcher {
            machine: self,
            input: string,
            bytes: string.as_bytes(),
        };
        let pos = matcher.skip_ws(0);
        let end = matcher.process_value(pos as u32, NodeId(0), state)?;
        let end = matcher.skip_ws(end);
        if end != matcher.bytes.len() as u32 {
            return Err(MatchError::TrailingData { pos: end });
        }

        for (set_index, &required) in self.set_required_counts.iter().enumerate() {
            state
                .result
                .match_results
                .set(set_index, state.set_counts[set_index] == required);
        }
        Ok(())
    }
}

struct Matcher<'m, 's> {
    machine: &'m MatchMachine,
    input: &'s str,
    bytes: &'s [u8],
}

impl Matcher<'_, '_> {
    fn skip_ws(&self, mut pos: u32) -> u32 {
        while let Some(byte) = self.byte_at(pos) {
            match byte {
                b' ' | b'\t' | b'\n' | b'\r' => pos += 1,
                _ => break,
            }
        }
        pos
    }

    #[inline]
    fn peek(&self, pos: u32) -> Result<u8, MatchError> {
        self.byte_at(pos).ok_or(MatchError::UnexpectedEof)
    }

    /// Scan one value starting at pos, descending only where the trie has
    /// matching children, then run the node's actions on the value's span.
    /// Returns the position just past the value.
    fn process_value(
        &self,
        pos: u32,
        node_index: NodeId,
        state: &mut MachineState,
    ) -> Result<u32, MatchError> {
        let node = &self.machine.nodes[node_index.index()];
        let first = self.peek(pos)?;
        let mut string_escaped = false;
        let end = match first {
            b'{' if !node.key_children.is_empty() || node.has_exhaustive => {
                self.walk_object(pos, node, state)?
            }
            b'[' if node.any_index_child.is_some()
                || !node.index_children.is_empty()
                || node.has_exhaustive =>
            {
                self.walk_array(pos, node, state)?
            }
            b'"' => {
                let (end, escaped) = self.validate_string(pos)?;
                string_escaped = escaped;
                end
            }
            _ => self.validate_value(pos, state)?,
        };
        if node.actions.start != node.actions.end {
            self.run_actions(node, range_u32(pos, end), string_escaped, state)?;
        }
        Ok(end)
    }

    // NOTE: monomorphizing the walks and run_actions over has_exhaustive
    // (const-generic true/false copies) was benchmarked and lost badly to
    // these runtime branches (up to +25% on small documents): the doubled
    // code pushed the hot loops apart in the icache.
    fn walk_object(
        &self,
        pos: u32,
        node: &Node,
        state: &mut MachineState,
    ) -> Result<u32, MatchError> {
        if node.has_exhaustive {
            self.reset_exhaustive(node, state);
        }
        let mut pos = self.skip_ws(pos + 1);
        if self.peek(pos)? == b'}' {
            return Ok(pos + 1);
        }
        loop {
            let byte = self.peek(pos)?;
            if byte != b'"' {
                return Err(MatchError::UnexpectedByte { pos, byte });
            }
            let key_start = pos;
            let (key_end, key_escaped) = self.validate_string(pos)?;
            let child = self.lookup_key(
                node,
                range_u32(key_start + 1, key_end - 1),
                key_escaped,
                state,
            )?;
            if node.has_exhaustive {
                self.flag_uncovered(node, child, state);
            }
            pos = self.skip_ws(key_end);
            let byte = self.peek(pos)?;
            if byte != b':' {
                return Err(MatchError::UnexpectedByte { pos, byte });
            }
            pos = self.skip_ws(pos + 1);
            pos = match child {
                Some(child) => self.process_value(pos, child, state)?,
                None => self.validate_value(pos, state)?,
            };
            pos = self.skip_ws(pos);
            match self.peek(pos)? {
                b',' => pos = self.skip_ws(pos + 1),
                b'}' => return Ok(pos + 1),
                byte => return Err(MatchError::UnexpectedByte { pos, byte }),
            }
        }
    }

    fn lookup_key(
        &self,
        node: &Node,
        content: Range<u32>,
        escaped: bool,
        state: &mut MachineState,
    ) -> Result<Option<NodeId>, MatchError> {
        let raw = if !escaped {
            &self.bytes[range_u32_to_usize(content)]
        } else {
            state.unescape_buf.clear();
            unescape_into(
                &self.input[range_u32_to_usize(content)],
                &mut state.unescape_buf,
                content.start,
            )?;
            state.unescape_buf.as_bytes()
        };
        // Linear scan: nodes rarely have more than a handful of keys, and the
        // length pre-check in slice equality rejects most candidates in one
        // comparison — measurably faster here than binary search.
        Ok(node
            .key_children
            .iter()
            .find(|(key, _)| key.as_bytes() == raw)
            .map(|&(_, child)| child))
    }

    /// Clears the violation bits of the node's exhaustive actions at the start
    /// of walking one container instance. The exhaustive helpers are #[cold]
    /// so patterns without exhaustive fields pay only the has_exhaustive
    /// branches, not the helpers' code in their walk loops.
    #[cold]
    fn reset_exhaustive(&self, node: &Node, state: &mut MachineState) {
        for action_index in range_u32_to_usize(node.actions) {
            if self.machine.actions[action_index].exhaustive.is_some() {
                state.exhaustive_violated.set(action_index, false);
            }
        }
    }

    /// Marks every exhaustive action of the node not covering the container
    /// member that resolved to `child` as violated (None: a member no field
    /// path descends). For arrays, pass the fixed-index child only: an element
    /// reached through any_index_child is never covered — same-set AnyIndex
    /// under an exhaustive container is rejected at compile time, so such an
    /// element can only belong to other sets.
    #[cold]
    fn flag_uncovered(&self, node: &Node, child: Option<NodeId>, state: &mut MachineState) {
        for action_index in range_u32_to_usize(node.actions) {
            if let Some(coverage) = &self.machine.actions[action_index].exhaustive
                && !child.is_some_and(|child| coverage.covered_nodes[child.index()])
            {
                state.exhaustive_violated.set(action_index, true);
            }
        }
    }

    fn walk_array(
        &self,
        pos: u32,
        node: &Node,
        state: &mut MachineState,
    ) -> Result<u32, MatchError> {
        if node.has_exhaustive {
            self.reset_exhaustive(node, state);
        }
        let mut pos = self.skip_ws(pos + 1);
        if self.peek(pos)? == b']' {
            return Ok(pos + 1);
        }
        // The <= 512 path is the likely one; walk_array_large is #[cold] so
        // this branch is laid out and predicted accordingly.
        if node.index_children.len() <= 512 {
            for index in 0..=u16::MAX {
                let fixed = node
                    .index_children
                    .index()
                    .iter()
                    .position(|&i| i == index)
                    .map(|found| node.index_children.child()[found]);
                if node.has_exhaustive {
                    self.flag_uncovered(node, fixed, state);
                }
                let child = fixed.or(node.any_index_child);
                pos = match child {
                    Some(child) => self.process_value(pos, child, state)?,
                    None => self.validate_value(pos, state)?,
                };
                pos = self.skip_ws(pos);
                match self.peek(pos)? {
                    b',' => pos = self.skip_ws(pos + 1),
                    b']' => return Ok(pos + 1),
                    byte => return Err(MatchError::UnexpectedByte { pos, byte }),
                }
            }
            self.walk_array_huge(pos, node, state, u16::MAX as u32)
        } else {
            self.walk_array_large(pos, node, state)
        }
    }

    #[cold]
    fn walk_array_large(
        &self,
        mut pos: u32,
        node: &Node,
        state: &mut MachineState,
    ) -> Result<u32, MatchError> {
        for index in 0..=u16::MAX {
            let fixed = node
                .index_children
                .index()
                .binary_search(&index)
                .ok()
                .map(|found| node.index_children.child()[found]);
            if node.has_exhaustive {
                self.flag_uncovered(node, fixed, state);
            }
            let child = fixed.or(node.any_index_child);
            pos = match child {
                Some(child) => self.process_value(pos, child, state)?,
                None => self.validate_value(pos, state)?,
            };
            pos = self.skip_ws(pos);
            match self.peek(pos)? {
                b',' => pos = self.skip_ws(pos + 1),
                b']' => return Ok(pos + 1),
                byte => return Err(MatchError::UnexpectedByte { pos, byte }),
            }
        }
        self.walk_array_huge(pos, node, state, u16::MAX as u32)
    }

    #[cold]
    fn walk_array_huge(
        &self,
        mut pos: u32,
        node: &Node,
        state: &mut MachineState,
        mut index: u32,
    ) -> Result<u32, MatchError> {
        // Every element from here on is beyond any fixed-index coverage.
        if node.has_exhaustive {
            self.flag_uncovered(node, None, state);
        }
        loop {
            let child = node.any_index_child;
            pos = match child {
                Some(child) => self.process_value(pos, child, state)?,
                None => self.validate_value(pos, state)?,
            };
            pos = self.skip_ws(pos);
            match self.peek(pos)? {
                b',' => {
                    pos = self.skip_ws(pos + 1);
                    index = index.saturating_add(1);
                }
                b']' => return Ok(pos + 1),
                byte => return Err(MatchError::UnexpectedByte { pos, byte }),
            }
        }
    }

    /// Fully validate one JSON value starting at pos without interpreting it,
    /// and return the position just past it. Container nesting is tracked in
    /// the state's bracket stack; exceeding its length is an error, so the
    /// stack length is the depth limit for regions the pattern skips.
    fn validate_value(&self, pos: u32, state: &mut MachineState) -> Result<u32, MatchError> {
        let limit = state.bracket_stack.len();
        let mut depth = 0usize;
        let mut i = pos;
        'value: loop {
            // A value is expected at i.
            match self.peek(i)? {
                b'{' => {
                    if depth == limit {
                        return Err(MatchError::DepthLimitExceeded { pos: i });
                    }
                    state.bracket_stack.set(depth, true);
                    depth += 1;
                    i = self.skip_ws(i + 1);
                    if self.peek(i)? == b'}' {
                        i += 1;
                        depth -= 1;
                    } else {
                        i = self.validate_key_colon(i)?;
                        continue 'value;
                    }
                }
                b'[' => {
                    if depth == limit {
                        return Err(MatchError::DepthLimitExceeded { pos: i });
                    }
                    state.bracket_stack.set(depth, false);
                    depth += 1;
                    i = self.skip_ws(i + 1);
                    if self.peek(i)? == b']' {
                        i += 1;
                        depth -= 1;
                    } else {
                        continue 'value;
                    }
                }
                b'"' => i = self.validate_string(i)?.0,
                b't' => i = self.expect_keyword(i, b"true")?,
                b'f' => i = self.expect_keyword(i, b"false")?,
                b'n' => i = self.expect_keyword(i, b"null")?,
                b'-' | b'0'..=b'9' => i = self.validate_number(i)?,
                byte => return Err(MatchError::UnexpectedByte { pos: i, byte }),
            }
            // A value just ended; close containers / consume separators.
            loop {
                if depth == 0 {
                    return Ok(i);
                }
                i = self.skip_ws(i);
                let is_object = state.bracket_stack[depth - 1];
                match self.peek(i)? {
                    b',' => {
                        i = self.skip_ws(i + 1);
                        if is_object {
                            i = self.validate_key_colon(i)?;
                        }
                        continue 'value;
                    }
                    b'}' if is_object => {
                        depth -= 1;
                        i += 1;
                    }
                    b']' if !is_object => {
                        depth -= 1;
                        i += 1;
                    }
                    byte => return Err(MatchError::UnexpectedByte { pos: i, byte }),
                }
            }
        }
    }

    /// At a member key: validate the key string and the ':' separator, and
    /// return the position of the member's value.
    fn validate_key_colon(&self, pos: u32) -> Result<u32, MatchError> {
        let byte = self.peek(pos)?;
        if byte != b'"' {
            return Err(MatchError::UnexpectedByte { pos, byte });
        }
        let (end, _) = self.validate_string(pos)?;
        let pos = self.skip_ws(end);
        let byte = self.peek(pos)?;
        if byte != b':' {
            return Err(MatchError::UnexpectedByte { pos, byte });
        }
        Ok(self.skip_ws(pos + 1))
    }

    #[inline]
    fn expect_keyword(&self, pos: u32, keyword: &[u8]) -> Result<u32, MatchError> {
        let end = (pos as usize) + keyword.len();
        let bytes = self
            .bytes
            .get(pos as usize..end)
            .ok_or(MatchError::UnexpectedEof)?;
        if bytes == keyword {
            Ok(end as u32)
        } else {
            Err(MatchError::UnexpectedByte {
                pos,
                byte: self.bytes[pos as usize],
            })
        }
    }

    #[inline]
    fn byte_at(&self, pos: u32) -> Option<u8> {
        self.bytes.get(pos as usize).copied()
    }

    /// Strict JSON number grammar: `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`.
    fn validate_number(&self, pos: u32) -> Result<u32, MatchError> {
        let mut i = pos;
        if self.byte_at(i) == Some(b'-') {
            i += 1;
        }
        match self.byte_at(i) {
            Some(b'0') => i += 1,
            Some(b'1'..=b'9') => {
                i += 1;
                while let Some(b'0'..=b'9') = self.byte_at(i) {
                    i += 1;
                }
            }
            _ => return Err(MatchError::InvalidNumber { pos }),
        }
        if self.byte_at(i) == Some(b'.') {
            i += 1;
            let digits = i;
            while let Some(b'0'..=b'9') = self.byte_at(i) {
                i += 1;
            }
            if i == digits {
                return Err(MatchError::InvalidNumber { pos });
            }
        }
        if let Some(b'e' | b'E') = self.byte_at(i) {
            i += 1;
            if let Some(b'+' | b'-') = self.byte_at(i) {
                i += 1;
            }
            let digits = i;
            while let Some(b'0'..=b'9') = self.byte_at(i) {
                i += 1;
            }
            if i == digits {
                return Err(MatchError::InvalidNumber { pos });
            }
        }
        Ok(i)
    }

    /// pos is at the opening quote. Fully validates the string content: escape
    /// syntax (including surrogate pairing, matching `unescape_into`) and no
    /// raw control characters. Returns (position past the closing quote,
    /// whether the content contains escape sequences).
    // NOTE: bulk scanning strategies (memchr2 and SWAR word-at-a-time) were
    // benchmarked here and lost to this byte loop by 5-70% on the generated
    // workload, whose strings are all 3-8 bytes: any per-string setup cost
    // outweighs the scan. Revisit only with a benchmark containing long
    // strings, where word-at-a-time skipping should win decisively.
    fn validate_string(&self, pos: u32) -> Result<(u32, bool), MatchError> {
        let mut i = pos + 1;
        let mut escaped = false;
        loop {
            let byte = self.peek(i)?;
            match byte {
                b'"' => return Ok((i + 1, escaped)),
                b'\\' => {
                    escaped = true;
                    i = self.validate_escape(i)?;
                }
                0x00..=0x1F => return Err(MatchError::UnexpectedByte { pos: i, byte }),
                _ => i += 1,
            }
        }
    }

    /// pos is at the backslash. Validates one escape sequence (including
    /// surrogate pairing, matching `unescape_into`) and returns the position
    /// just past it. Escapes are rare in typical input, so this is marked cold
    /// to keep validate_string's byte loop small; escape-dense workloads pay
    /// a call per escape.
    #[cold]
    fn validate_escape(&self, pos: u32) -> Result<u32, MatchError> {
        let escape_pos = pos;
        let code = self.peek(pos + 1)?;
        let mut i = pos + 2;
        match code {
            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
            b'u' => {
                let unit = parse_hex4(self.bytes, i)
                    .ok_or(MatchError::InvalidEscape { pos: escape_pos })?;
                i += 4;
                if (0xD800..=0xDBFF).contains(&unit) {
                    if self.byte_at(i) != Some(b'\\') || self.byte_at(i + 1) != Some(b'u') {
                        return Err(MatchError::InvalidEscape { pos: escape_pos });
                    }
                    let low = parse_hex4(self.bytes, i + 2)
                        .ok_or(MatchError::InvalidEscape { pos: escape_pos })?;
                    if !(0xDC00..=0xDFFF).contains(&low) {
                        return Err(MatchError::InvalidEscape { pos: escape_pos });
                    }
                    i += 6;
                } else if (0xDC00..=0xDFFF).contains(&unit) {
                    return Err(MatchError::InvalidEscape { pos: escape_pos });
                }
            }
            _ => return Err(MatchError::InvalidEscape { pos: escape_pos }),
        }
        Ok(i)
    }

    fn run_actions(
        &self,
        node: &Node,
        range: Range<u32>,
        string_escaped: bool,
        state: &mut MachineState,
    ) -> Result<(), MatchError> {
        let actions = &self.machine.actions[range_u32_to_usize(node.actions)];
        let first = self.bytes[range.start as usize];
        // Whether unescape_buf currently holds this value's unescaped content.
        let mut buf_ready = false;
        // A violated exhaustive container stays unsatisfied. Non-container
        // values have no members, so they are trivially exhaustive (their walk
        // never ran and the violation bits are stale, hence the container
        // gate). Violation bits are only ever set for exhaustive actions, and
        // exhaustive nodes always walk containers, so the bit alone decides —
        // no action field load needed.
        let check_exhaustive = node.has_exhaustive && matches!(first, b'{' | b'[');
        for (offset, action) in actions.iter().enumerate() {
            if state.satisfied[action.field_bit as usize] {
                continue;
            }
            if check_exhaustive && state.exhaustive_violated[node.actions.start as usize + offset] {
                continue;
            }
            let type_ok = match action.type_check {
                MachineFieldType::Object => first == b'{',
                MachineFieldType::Array => first == b'[',
                MachineFieldType::String => first == b'"',
                MachineFieldType::Number => matches!(first, b'-' | b'0'..=b'9'),
                MachineFieldType::Bool => matches!(first, b't' | b'f'),
                MachineFieldType::Null => first == b'n',
                MachineFieldType::Literal { len, lsb, msb } => {
                    // let pool_slice = &self.machine.literal_pool[u24_to_usize(lsb, msb)..];
                    // let (len, off) = u32::decode_var(&pool_slice).unwrap();
                    let off = u24_to_usize(lsb, msb);
                    let literal = &self.machine.literal_pool[off..off + len as usize];
                    literal == &self.bytes[range_u32_to_usize(range)]
                }
                MachineFieldType::Any => true,
            };
            if !type_ok {
                continue;
            }

            if let Some(predicate) = &action.predicate {
                // Predicates run on the parsed value text: for strings the
                // unescaped content, for everything else the raw span.
                let content = if first == b'"' {
                    Range {
                        start: range.start + 1,
                        end: range.end - 1,
                    }
                } else {
                    range
                };
                let use_buf = first == b'"' && string_escaped;
                if use_buf && !buf_ready {
                    state.unescape_buf.clear();
                    unescape_into(
                        &self.input[range_u32_to_usize(content)],
                        &mut state.unescape_buf,
                        content.start,
                    )?;
                    buf_ready = true;
                }
                let haystack: &str = if use_buf {
                    &state.unescape_buf
                } else {
                    &self.input[range_u32_to_usize(content)]
                };
                let locs = &mut state.capture_locs[action.predicate_loc as usize];
                if predicate.captures_read(locs, haystack).is_none() {
                    continue;
                }
                for &(group_index, capture_index) in &action.predicate_groups {
                    let value = locs
                        .get(group_index as usize)
                        .map(|(group_start, group_end)| {
                            CaptureValue::PredicateCapture(if use_buf {
                                UnescapedString::Owned(CompactString::from(
                                    &state.unescape_buf[group_start..group_end],
                                ))
                            } else {
                                UnescapedString::Borrowed(Range {
                                    start: content.start + group_start as u32,
                                    end: content.start + group_end as u32,
                                })
                            })
                        });
                    state.result.capture_values[(capture_index.get() - 1) as usize] = value;
                }
            }

            state.satisfied.set(action.field_bit as usize, true);
            state.set_counts[action.set_index as usize] += 1;

            if let Some(capture_index) = action.value_capture {
                let value =
                    self.build_capture(first, range, string_escaped, &mut buf_ready, state)?;
                state.result.capture_values[(capture_index.get() - 1) as usize] = Some(value);
            }
        }
        Ok(())
    }

    fn build_capture(
        &self,
        first: u8,
        range: Range<u32>,
        string_escaped: bool,
        buf_ready: &mut bool,
        state: &mut MachineState,
    ) -> Result<CaptureValue, MatchError> {
        Ok(match first {
            b'{' => CaptureValue::Object(range),
            b'[' => CaptureValue::Array(range),
            b'"' => {
                let content = Range {
                    start: range.start + 1,
                    end: range.end - 1,
                };
                if !string_escaped {
                    CaptureValue::String(UnescapedString::Borrowed(content))
                } else {
                    if !*buf_ready {
                        state.unescape_buf.clear();
                        unescape_into(
                            &self.input[range_u32_to_usize(content)],
                            &mut state.unescape_buf,
                            content.start,
                        )?;
                        *buf_ready = true;
                    }
                    CaptureValue::String(UnescapedString::Owned(CompactString::from(
                        state.unescape_buf.as_str(),
                    )))
                }
            }
            b't' => CaptureValue::Bool(true),
            b'f' => CaptureValue::Bool(false),
            b'n' => CaptureValue::Null,
            _ => {
                let text = &self.input[range_u32_to_usize(range)];
                match text.parse::<f64>() {
                    // JSON has no infinities: literals that overflow f64
                    // (e.g. 1e999) fail the match rather than capture ±inf.
                    Ok(number) if number.is_finite() => CaptureValue::Number(number),
                    Ok(_) => return Err(MatchError::NumberOutOfRange { pos: range.start }),
                    Err(_) => return Err(MatchError::InvalidNumber { pos: range.start }),
                }
            }
        })
    }
}

fn unescape_into(raw: &str, out: &mut String, base_pos: u32) -> Result<(), MatchError> {
    let bytes = raw.as_bytes();
    let mut chunk_start = 0;
    let mut i: u32 = 0;
    while i < bytes.len() as u32 {
        if bytes[i as usize] != b'\\' {
            i += 1;
            continue;
        }
        out.push_str(&raw[chunk_start as usize..i as usize]);
        let escape_pos = base_pos + i;
        let code = *bytes
            .get(i as usize + 1)
            .ok_or(MatchError::InvalidEscape { pos: escape_pos })?;
        i += 2;
        match code {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{0008}'),
            b'f' => out.push('\u{000C}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
                let unit =
                    parse_hex4(bytes, i).ok_or(MatchError::InvalidEscape { pos: escape_pos })?;
                i += 4;
                let ch = if (0xD800..=0xDBFF).contains(&unit) {
                    if bytes.get(i as usize) != Some(&b'\\')
                        || bytes.get(i as usize + 1) != Some(&b'u')
                    {
                        return Err(MatchError::InvalidEscape { pos: escape_pos });
                    }
                    let low = parse_hex4(bytes, i + 2)
                        .ok_or(MatchError::InvalidEscape { pos: escape_pos })?;
                    if !(0xDC00..=0xDFFF).contains(&low) {
                        return Err(MatchError::InvalidEscape { pos: escape_pos });
                    }
                    i += 6;
                    let combined =
                        0x10000 + (((unit as u32) - 0xD800) << 10) + ((low as u32) - 0xDC00);
                    char::from_u32(combined).ok_or(MatchError::InvalidEscape { pos: escape_pos })?
                } else {
                    char::from_u32(unit as u32)
                        .ok_or(MatchError::InvalidEscape { pos: escape_pos })?
                };
                out.push(ch);
            }
            _ => return Err(MatchError::InvalidEscape { pos: escape_pos }),
        }
        chunk_start = i;
    }
    out.push_str(&raw[chunk_start as usize..]);
    Ok(())
}

fn parse_hex4(bytes: &[u8], pos: u32) -> Option<u16> {
    let bytes = bytes.get(pos as usize..pos as usize + 4)?;
    let mut value: u16 = 0;
    for &byte in bytes {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        };
        value = (value << 4) | digit as u16;
    }
    Some(value)
}
