#![deny(unused_must_use)]

use bitvec::{bitbox, boxed::BitBox, order::Lsb0};
use compact_str::CompactString;
use regex::{CaptureLocations, Regex};
use std::{assert_matches, range::Range};

pub mod testing;

pub struct MatchSet<'a> {
    pub field_matches: &'a [FieldMatch<'a>],
}

pub struct FieldMatch<'a> {
    pub path: &'a [PathSegment<'a>],
    pub r#type: FieldType,
    pub predicate: Option<Regex>,
    pub capture: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum PathSegment<'a> {
    Key(&'a str),
    /// Fixed array index.
    Index(u32),
    /// First array element whose continuation satisfies this field.
    /// Independent per field: two fields sharing an AnyIndex prefix may be
    /// satisfied by different elements.
    AnyIndex,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug, PartialEq)]
pub enum CaptureValue {
    NotCaptured,
    PredicateCapture(UnescapedString),
    Object(Range<u32>),
    Array(Range<u32>),
    String(UnescapedString),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Debug, PartialEq)]
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

pub struct CaptureCallbackArgs<'a> {
    pub match_set_index: u32,
    pub field_index: u32,
    pub predicate_capture_name: Option<&'a str>,
    pub capture_index_in_set: u32,
    pub capture_index_in_machine: u32,
}

#[derive(Debug)]
pub enum CompileError {}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MatchError {
    #[error("unexpected byte 0x{byte:02x} at offset {pos}")]
    UnexpectedByte { pos: u32, byte: u8 },
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("trailing data at offset {pos}")]
    TrailingData { pos: u32 },
    #[error("invalid number at offset {pos}")]
    InvalidNumber { pos: u32 },
    #[error("invalid string escape at offset {pos}")]
    InvalidEscape { pos: u32 },
    #[error("nesting depth limit exceeded at offset {pos}")]
    DepthLimitExceeded { pos: u32 },
}

#[derive(Clone)]
struct Action {
    set_index: u32,
    /// Global field id across all sets; indexes MachineState.satisfied.
    field_bit: u32,
    type_check: FieldType,
    predicate: Option<Regex>,
    /// Index into MachineState.capture_locs; u32::MAX when predicate is None.
    predicate_loc: u32,
    /// (regex group index, machine capture index) for named groups not starting with '_'.
    predicate_groups: Box<[(u32, u32)]>,
    value_capture: Option<u32>,
}

struct Node {
    key_children: Box<[(CompactString, u32)]>,
    /// Sorted by index. When a node also has a wildcard child, each fixed-index
    /// subtree already contains a merged copy of the wildcard subtree, so every
    /// array element resolves to at most one node.
    index_children: Box<[(u32, u32)]>,
    any_index_child: Option<u32>,
    actions: Range<u32>,
}

pub struct MatchMachine {
    captures_length: u32,
    fields_length: u32,
    predicates_length: u32,
    set_required_counts: Box<[u32]>,
    nodes: Box<[Node]>,
    actions: Box<[Action]>,
}

pub struct MachineResult {
    capture_values: Box<[CaptureValue]>,
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
    pub fn capture(&self, machine_capture_index: u32) -> &CaptureValue {
        &self.capture_values[machine_capture_index as usize]
    }
    pub fn captures(&self) -> &[CaptureValue] {
        &self.capture_values
    }
}

pub const DEFAULT_DEPTH_LIMIT: usize = 128;

pub struct MachineState {
    pub result: MachineResult,
    satisfied: BitBox,
    set_counts: Box<[u32]>,
    capture_locs: Box<[CaptureLocations]>,
    unescape_buf: String,
    /// One bit per open container while validating skipped regions
    /// (1 = object, 0 = array); its length is the nesting depth limit.
    bracket_stack: BitBox,
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
    key_children: Vec<(CompactString, u32)>,
    index_children: Vec<(u32, u32)>,
    any_index_child: Option<u32>,
    actions: Vec<Action>,
}

fn push_node(nodes: &mut Vec<NodeBuild>) -> u32 {
    let index = nodes.len() as u32;
    nodes.push(NodeBuild::default());
    index
}

fn get_or_create_child(nodes: &mut Vec<NodeBuild>, parent: u32, segment: PathSegment) -> u32 {
    match segment {
        PathSegment::Key(key) => {
            let existing = nodes[parent as usize]
                .key_children
                .iter()
                .find(|(k, _)| k.as_str() == key)
                .map(|&(_, child)| child);
            existing.unwrap_or_else(|| {
                let child = push_node(nodes);
                nodes[parent as usize]
                    .key_children
                    .push((CompactString::from(key), child));
                child
            })
        }
        PathSegment::Index(index) => {
            let existing = nodes[parent as usize]
                .index_children
                .iter()
                .find(|&&(i, _)| i == index)
                .map(|&(_, child)| child);
            existing.unwrap_or_else(|| {
                let child = push_node(nodes);
                nodes[parent as usize].index_children.push((index, child));
                child
            })
        }
        PathSegment::AnyIndex => match nodes[parent as usize].any_index_child {
            Some(child) => child,
            None => {
                let child = push_node(nodes);
                nodes[parent as usize].any_index_child = Some(child);
                child
            }
        },
    }
}

/// Union the src subtree into the dst subtree, cloning actions. Duplicated
/// actions share their field_bit, so the satisfied bitset dedupes them at runtime.
fn merge_subtree(nodes: &mut Vec<NodeBuild>, dst: u32, src: u32) {
    let src_actions = nodes[src as usize].actions.clone();
    nodes[dst as usize].actions.extend(src_actions);
    let src_keys = nodes[src as usize].key_children.clone();
    for (key, src_child) in src_keys {
        let dst_child = get_or_create_child(nodes, dst, PathSegment::Key(key.as_str()));
        merge_subtree(nodes, dst_child, src_child);
    }
    let src_indices = nodes[src as usize].index_children.clone();
    for (index, src_child) in src_indices {
        let dst_child = get_or_create_child(nodes, dst, PathSegment::Index(index));
        merge_subtree(nodes, dst_child, src_child);
    }
    if let Some(src_any) = nodes[src as usize].any_index_child {
        let dst_any = get_or_create_child(nodes, dst, PathSegment::AnyIndex);
        merge_subtree(nodes, dst_any, src_any);
    }
}

/// Wherever a node has both fixed-index children and a wildcard child, merge the
/// wildcard subtree into each fixed subtree so array elements resolve to one node.
fn merge_wildcards(nodes: &mut Vec<NodeBuild>, node: u32) {
    if let Some(any) = nodes[node as usize].any_index_child {
        let fixed: Vec<u32> = nodes[node as usize]
            .index_children
            .iter()
            .map(|&(_, c)| c)
            .collect();
        for child in fixed {
            merge_subtree(nodes, child, any);
        }
    }
    let mut children: Vec<u32> = nodes[node as usize]
        .key_children
        .iter()
        .map(|&(_, c)| c)
        .collect();
    children.extend(nodes[node as usize].index_children.iter().map(|&(_, c)| c));
    children.extend(nodes[node as usize].any_index_child);
    for child in children {
        merge_wildcards(nodes, child);
    }
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
                capture_values: vec![CaptureValue::NotCaptured; self.captures_length as usize]
                    .into_boxed_slice(),
                match_results: bitbox![usize, Lsb0; 0; self.set_required_counts.len()],
            },
            satisfied: bitbox![usize, Lsb0; 0; self.fields_length as usize],
            set_counts: vec![0u32; self.set_required_counts.len()].into_boxed_slice(),
            capture_locs: locs.into_iter().map(|slot| slot.unwrap()).collect(),
            unescape_buf: String::new(),
            bracket_stack: bitbox![usize, Lsb0; 0; DEFAULT_DEPTH_LIMIT],
        }
    }

    pub fn compile<'a>(
        match_sets: impl Iterator<Item = MatchSet<'a>>,
        mut capture_index_callback: impl FnMut(CaptureCallbackArgs),
    ) -> Result<MatchMachine, CompileError> {
        let mut nodes: Vec<NodeBuild> = vec![NodeBuild::default()];
        let mut set_required_counts: Vec<u32> = Vec::new();
        let mut next_machine_capture_index: u32 = 0;
        let mut next_field_bit: u32 = 0;
        let mut next_predicate_loc: u32 = 0;

        for (set_index, set) in match_sets.enumerate() {
            set_required_counts.push(set.field_matches.len() as u32);
            let mut next_set_capture_index: u32 = 0;
            for (field_index, field) in set.field_matches.iter().enumerate() {
                let mut node: u32 = 0;
                for &segment in field.path {
                    node = get_or_create_child(&mut nodes, node, segment);
                }

                let value_capture = if field.capture {
                    capture_index_callback(CaptureCallbackArgs {
                        match_set_index: set_index as u32,
                        field_index: field_index as u32,
                        predicate_capture_name: None,
                        capture_index_in_set: next_set_capture_index,
                        capture_index_in_machine: next_machine_capture_index,
                    });
                    let index = next_machine_capture_index;
                    next_machine_capture_index += 1;
                    next_set_capture_index += 1;
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
                                capture_index_in_set: next_set_capture_index,
                                capture_index_in_machine: next_machine_capture_index,
                            });
                            predicate_groups.push((group_index as u32, next_machine_capture_index));
                            next_machine_capture_index += 1;
                            next_set_capture_index += 1;
                        }
                    }
                    predicate_loc = next_predicate_loc;
                    next_predicate_loc += 1;
                }

                nodes[node as usize].actions.push(Action {
                    set_index: set_index as u32,
                    field_bit: next_field_bit,
                    type_check: field.r#type.clone(),
                    predicate: field.predicate.clone(),
                    predicate_loc,
                    predicate_groups: predicate_groups.into_boxed_slice(),
                    value_capture,
                });
                next_field_bit += 1;
            }
        }

        merge_wildcards(&mut nodes, 0);

        let mut actions: Vec<Action> = Vec::new();
        let mut final_nodes: Vec<Node> = Vec::with_capacity(nodes.len());
        for mut build in nodes {
            let start = actions.len() as u32;
            actions.append(&mut build.actions);
            build
                .index_children
                .sort_unstable_by_key(|&(index, _)| index);
            final_nodes.push(Node {
                key_children: build.key_children.into_boxed_slice(),
                index_children: build.index_children.into_boxed_slice(),
                any_index_child: build.any_index_child,
                actions: Range {
                    start,
                    end: actions.len() as u32,
                },
            });
        }

        Ok(MatchMachine {
            captures_length: next_machine_capture_index,
            fields_length: next_field_bit,
            predicates_length: next_predicate_loc,
            set_required_counts: set_required_counts.into_boxed_slice(),
            nodes: final_nodes.into_boxed_slice(),
            actions: actions.into_boxed_slice(),
        })
    }

    pub fn match_string(&self, string: &str, state: &mut MachineState) -> Result<(), MatchError> {
        const MAX_STR_LEN: usize = u32::MAX as usize;
        assert_matches!(string.len(), 0..MAX_STR_LEN);

        state.result.capture_values.fill(CaptureValue::NotCaptured);
        state.result.match_results.fill(false);
        state.satisfied.fill(false);
        state.set_counts.fill(0);

        let matcher = Matcher {
            machine: self,
            input: string,
            bytes: string.as_bytes(),
        };
        let pos = matcher.skip_ws(0);
        let end = matcher.process_value(pos as u32, 0, state)?;
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
        node_index: u32,
        state: &mut MachineState,
    ) -> Result<u32, MatchError> {
        let node = &self.machine.nodes[node_index as usize];
        let first = self.peek(pos)?;
        let mut string_escaped = false;
        let end = match first {
            b'{' if !node.key_children.is_empty() => self.walk_object(pos, node, state)?,
            b'[' if node.any_index_child.is_some() || !node.index_children.is_empty() => {
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

    fn walk_object(
        &self,
        pos: u32,
        node: &Node,
        state: &mut MachineState,
    ) -> Result<u32, MatchError> {
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
    ) -> Result<Option<u32>, MatchError> {
        if !escaped {
            let raw = &self.bytes[range_u32_to_usize(content)];
            for (key, child) in &node.key_children {
                if key.as_bytes() == raw {
                    return Ok(Some(*child));
                }
            }
        } else {
            state.unescape_buf.clear();
            unescape_into(
                &self.input[range_u32_to_usize(content)],
                &mut state.unescape_buf,
                content.start,
            )?;
            for (key, child) in &node.key_children {
                if key.as_str() == state.unescape_buf {
                    return Ok(Some(*child));
                }
            }
        }
        Ok(None)
    }

    fn walk_array(
        &self,
        pos: u32,
        node: &Node,
        state: &mut MachineState,
    ) -> Result<u32, MatchError> {
        let mut pos = self.skip_ws(pos + 1);
        if self.peek(pos)? == b']' {
            return Ok(pos + 1);
        }
        let mut index: u32 = 0;
        loop {
            let child = match node
                .index_children
                .binary_search_by_key(&index, |&(i, _)| i)
            {
                Ok(found) => Some(node.index_children[found].1),
                Err(_) => node.any_index_child,
            };
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
                byte @ (b'{' | b'[') => {
                    if depth == limit {
                        return Err(MatchError::DepthLimitExceeded { pos: i });
                    }
                    let is_object = byte == b'{';
                    state.bracket_stack.set(depth, is_object);
                    depth += 1;
                    i = self.skip_ws(i + 1);
                    if self.peek(i)? == (if is_object { b'}' } else { b']' }) {
                        i += 1;
                        depth -= 1;
                    } else if is_object {
                        i = self.validate_key_colon(i)?;
                        continue 'value;
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

    fn expect_keyword(&self, pos: u32, keyword: &[u8]) -> Result<u32, MatchError> {
        let end = pos + keyword.len() as u32;
        if (self.bytes.len() as u32) < end {
            return Err(MatchError::UnexpectedEof);
        }
        if &self.bytes[pos as usize..end as usize] == keyword {
            Ok(end)
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
    fn validate_string(&self, pos: u32) -> Result<(u32, bool), MatchError> {
        let mut i = pos + 1;
        let mut escaped = false;
        loop {
            let byte = self.peek(i)?;
            match byte {
                b'"' => return Ok((i + 1, escaped)),
                b'\\' => {
                    escaped = true;
                    let escape_pos = i;
                    let code = self.peek(i + 1)?;
                    i += 2;
                    match code {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                        b'u' => {
                            let unit = parse_hex4(self.bytes, i)
                                .ok_or(MatchError::InvalidEscape { pos: escape_pos })?;
                            i += 4;
                            if (0xD800..=0xDBFF).contains(&unit) {
                                if self.byte_at(i) != Some(b'\\')
                                    || self.byte_at(i + 1) != Some(b'u')
                                {
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
                }
                0x00..=0x1F => return Err(MatchError::UnexpectedByte { pos: i, byte }),
                _ => i += 1,
            }
        }
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
        for action in actions {
            if state.satisfied[action.field_bit as usize] {
                continue;
            }
            let type_ok = match &action.type_check {
                FieldType::Object => first == b'{',
                FieldType::Array => first == b'[',
                FieldType::String => first == b'"',
                FieldType::Number => matches!(first, b'-' | b'0'..=b'9'),
                FieldType::Bool => matches!(first, b't' | b'f'),
                FieldType::Null => first == b'n',
                FieldType::Literal(literal) => {
                    literal.as_bytes() == &self.bytes[range_u32_to_usize(range)]
                }
                FieldType::Any => true,
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
                    let value = match locs.get(group_index as usize) {
                        Some((group_start, group_end)) => {
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
                        }
                        None => CaptureValue::NotCaptured,
                    };
                    state.result.capture_values[capture_index as usize] = value;
                }
            }

            state.satisfied.set(action.field_bit as usize, true);
            state.set_counts[action.set_index as usize] += 1;

            if let Some(capture_index) = action.value_capture {
                let value =
                    self.build_capture(first, range, string_escaped, &mut buf_ready, state)?;
                state.result.capture_values[capture_index as usize] = value;
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
                    Ok(number) => CaptureValue::Number(number),
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
