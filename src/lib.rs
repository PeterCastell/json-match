#![deny(unused_must_use)]

use compact_str::CompactString;
use regex::Regex;
use std::{collections::HashMap, range::Range};
use bitvec::{bitbox, boxed::BitBox, order::Lsb0};

pub mod testing;


pub struct MatchSet<'a> {
    pub field_matches: &'a [FieldMatch<'a>],
    pub capture_map: HashMap<CompactString, u32>
}

pub struct FieldMatch<'a> {
    pub path: &'a [&'a str],
    pub r#type: FieldType<'a>,
    pub predicate: Option<Regex>,
    pub capture: bool
}

pub enum FieldType<'a> {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
    Literal(&'a str)
}

#[derive(Clone)]
pub enum CaptureValue {
    NotCaptured,
    Object(Range<usize>),
    Array(Range<usize>),
    String(UnescapedString),
    Number(f64),
    Bool(bool),
    Null
}

#[derive(Clone)]
pub enum UnescapedString {
    Borrowed(Range<usize>),
    Owned(CompactString)
}


pub struct MatchMachine {
    // things here
    captures_length: u32,
    offsets: Box<[u32]>,
}

pub struct CaptureCallbackArgs<'a> {
    match_set_index: u32,
    field_index: u32,
    predicate_capture_name: Option<&'a str>,
    capture_index_in_set: u32,
    capture_index_in_machine: u32
}

pub struct MachineResult {
    capture_values: Box<[CaptureValue]>,
    match_results: BitBox
}

pub enum CompileError {

}

impl MatchMachine {
    pub fn num_match_sets(&self) -> u32 {
        self.offsets.len() as u32
    }
    pub fn allocate_result(&self) -> MachineResult {
        return MachineResult {
            capture_values: vec![CaptureValue::NotCaptured; self.captures_length as usize].into_boxed_slice(),
            match_results: bitbox![usize, Lsb0; 0; self.num_match_sets() as usize]
        }
    }

    pub fn compile<'a>(match_set: impl Iterator<Item = MatchSet<'a>>, mut capture_index_callback: impl FnMut(CaptureCallbackArgs)) -> Result<MatchMachine, CompileError> {
        /* invariants to validate:
            
        */
    }

    pub fn match_string(&self, string: &str) {
        
    }
}