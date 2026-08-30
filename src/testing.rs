// use crate::*;
// use rand::{RngExt, SeedableRng};
// use std::collections::HashSet;

// use rand::distr::Alphanumeric;
// use rand::rngs::StdRng;

// pub fn generate_test_json(
//     pattern: ObjectMatch<'_>,
//     closeness: f64,
//     bloat: f64,
//     rng: &mut StdRng,
// ) -> String {
//     let bloat = if bloat.is_finite() { bloat.max(0.0) } else { 0.0 };
//     fn collect(fields: ObjectMatch<'_>, prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
//         for (i, field) in fields.iter().enumerate() {
//             prefix.push(i);
//             out.push(prefix.clone());
//             if let JsonType::ObjectMatch(inner) = &field.r#type {
//                 collect(inner, prefix, out);
//             }
//             prefix.pop();
//         }
//     }
//     let mut nodes = Vec::new();
//     collect(pattern, &mut Vec::new(), &mut nodes);
//     let total = nodes.len();
//     let k = (closeness.clamp(0.0, 1.0) * total as f64).floor() as usize;
//     let mut selected: HashSet<Vec<usize>> = HashSet::with_capacity(k);
//     let mut available: Vec<Vec<usize>> =
//         nodes.iter().filter(|p| p.len() == 1).cloned().collect();
//     for _ in 0..k {
//         let picked = available.swap_remove(rng.random_range(0..available.len()));
//         for node in &nodes {
//             if node.len() == picked.len() + 1 && node[..picked.len()] == picked[..] {
//                 available.push(node.clone());
//             }
//         }
//         selected.insert(picked);
//     }
//     let mut r#gen = Gen { selected, rng, bloat };
//     r#gen.object(pattern, &mut Vec::new(), 1)
// }
// struct Gen<'r> {
//     selected: HashSet<Vec<usize>>,
//     rng: &'r mut StdRng,
//     bloat: f64,
// }
// impl Gen<'_> {
//     fn object(&mut self, fields: ObjectMatch<'_>, path: &mut Vec<usize>, level: usize) -> String {
//         let mut members = Vec::new();
//         for (i, field) in fields.iter().enumerate() {
//             path.push(i);
//             if self.selected.contains(&*path) {
//                 let value = self.value(&field.r#type, path, level);
//                 members.push(format!("{:?}:{}", field.name, value));
//             }
//             path.pop();
//         }
//         let spine_len = (level as f64 * self.bloat).floor() as usize;
//         let extra = (self.bloat * fields.len() as f64 / 2.0).floor() as usize;
//         let mut noise_values = Vec::new();
//         if spine_len > 0 {
//             noise_values.push(self.chain_value(spine_len));
//         }
//         while noise_values.len() < extra {
//             let d = self.rng.random_range(0..=2);
//             noise_values.push(self.chain_value(d));
//         }
//         for value in noise_values {
//             let name = loop {
//                 let candidate = rand_name(self.rng);
//                 if !fields.iter().any(|f| f.name == candidate) {
//                     break candidate;
//                 }
//             };
//             let member = format!("{:?}:{}", name, value);
//             let at = self.rng.random_range(0..=members.len());
//             members.insert(at, member);
//         }
//         format!("{{{}}}", members.join(","))
//     }
//     fn value(&mut self, ty: &JsonType<'_>, path: &mut Vec<usize>, level: usize) -> String {
//         match ty {
//             JsonType::String => format!("{:?}", rand_name(self.rng)),
//             JsonType::Number => noise_number(self.rng),
//             JsonType::Bool => self.rng.random_bool(0.5).to_string(),
//             JsonType::Object => {
//                 if self.bloat == 0.0 {
//                     "{}".to_string()
//                 } else {
//                     let len = self.rng.random_range(1..=2);
//                     self.chain_object(len)
//                 }
//             }
//             JsonType::Array => {
//                 if self.bloat == 0.0 {
//                     "[]".to_string()
//                 } else {
//                     let len = self.rng.random_range(1..=2);
//                     self.chain_array(len)
//                 }
//             }
//             JsonType::Literal(lit) => lit.to_string(),
//             JsonType::ObjectMatch(inner) => self.object(inner, path, level + 1),
//         }
//     }
//     fn chain_value(&mut self, len: usize) -> String {
//         if len == 0 {
//             self.primitive()
//         } else if self.rng.random_bool(0.5) {
//             self.chain_array(len)
//         } else {
//             self.chain_object(len)
//         }
//     }
//     fn chain_array(&mut self, len: usize) -> String {
//         let child = self.chain_value(len - 1);
//         let mut items: Vec<String> =
//             (0..self.sibling_count()).map(|_| self.primitive()).collect();
//         let at = self.rng.random_range(0..=items.len());
//         items.insert(at, child);
//         format!("[{}]", items.join(","))
//     }
//     fn chain_object(&mut self, len: usize) -> String {
//         let child = self.chain_value(len - 1);
//         let mut members: Vec<String> = (0..self.sibling_count())
//             .map(|_| format!("{:?}:{}", rand_name(self.rng), self.primitive()))
//             .collect();
//         let at = self.rng.random_range(0..=members.len());
//         members.insert(at, format!("{:?}:{}", rand_name(self.rng), child));
//         format!("{{{}}}", members.join(","))
//     }
//     fn sibling_count(&mut self) -> usize {
//         let cap = ((self.bloat * 2.0).ceil() as usize).min(5);
//         self.rng.random_range(0..=cap)
//     }
//     fn primitive(&mut self) -> String {
//         match self.rng.random_range(0..4) {
//             0 => format!("{:?}", rand_name(self.rng)),
//             1 => noise_number(self.rng),
//             2 => self.rng.random_bool(0.5).to_string(),
//             _ => "null".to_string(),
//         }
//     }
// }
// fn rand_name(rng: &mut StdRng) -> String {
//     let len = rng.random_range(3..=8);
//     (0..len).map(|_| char::from(rng.sample(Alphanumeric))).collect()
// }
// fn noise_number(rng: &mut StdRng) -> String {
//     if rng.random_bool(0.5) {
//         rng.random_range(-1_000..1_000i64).to_string()
//     } else {
//         format!("{:.2}", rng.random_range(-1_000.0..1_000.0f64))
//     }
// }

// pub static TEST_STRUCTURE: ObjectMatch = &[
//     Field {
//         name: "foo",
//         r#type: JsonType::String,
//         predicate: None,
//         capture: Some("foo"),
//     },
//     Field {
//         name: "a",
//         r#type: JsonType::ObjectMatch(&[
//             Field {
//                 name: "b",
//                 r#type: JsonType::Bool,
//                 predicate: None,
//                 capture: Some("bar"),
//             },
//             Field {
//                 name: "c",
//                 r#type: JsonType::ObjectMatch(&[
//                     Field {
//                         name: "d",
//                         r#type: JsonType::String,
//                         predicate: None,
//                         capture: Some("hello"),
//                     },
//                 ]),
//                 predicate: None,
//                 capture: Some("c_obj"),
//             }
//         ]),
//         predicate: None,
//         capture: None,
//     }
// ];
// #[test]
// fn print_regex_pattern() {
//     let string = create_regex_pattern_string(TEST_STRUCTURE).unwrap();
//     println!("{}", string);
// }

// #[test]
// fn print_test_json() {
//     let mut rng = StdRng::seed_from_u64(0xB00B5);
//     let json = generate_test_json(TEST_STRUCTURE, 1.0, 1.0, &mut rng);
//     println!("{}", json);
// }

// #[test]
// fn capture() {
//     let regex = fancy_regex::Regex::new(&create_regex_pattern_string(testing::TEST_STRUCTURE).unwrap()).unwrap();
//     let mut rng = rand::rngs::StdRng::seed_from_u64(0xB00B5);
//     let test_str = &testing::generate_test_json(testing::TEST_STRUCTURE, 1.0, 1.0, &mut rng);
//     // regex.captures(test_str).unwrap();
// }