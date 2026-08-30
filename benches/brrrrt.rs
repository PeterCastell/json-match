// use std::hint::black_box;

// use criterion::{Criterion, criterion_group, criterion_main};
// use json_regex::*;
// use rand::SeedableRng;

// criterion_group!(benches, criterion_benchmark);
// criterion_main!(benches);
// fn criterion_benchmark(c: &mut Criterion) {
//     rand::rngs::StdRng::seed_from_u64(0);
//     c.bench_function("speed_test", |b|{
//         let regex = fancy_regex::Regex::new(&create_regex_pattern_string(testing::TEST_STRUCTURE).unwrap()).unwrap();
//         let mut rng = rand::rngs::StdRng::seed_from_u64(0);
//         let test_str = &testing::generate_test_json(testing::TEST_STRUCTURE, 1.0, 1.0, &mut rng);
//         b.iter(||{
//             black_box(regex.captures(black_box(test_str))).unwrap();
//         });
//     });
// }