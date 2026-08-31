use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use json_match::testing::{generate_test_json, test_fields};
use json_match::{MatchMachine, MatchSet};
use rand::SeedableRng;
use rand::rngs::StdRng;
use std::hint::black_box;

fn bench_match(c: &mut Criterion) {
    let fields = test_fields();
    let machine = MatchMachine::compile(
        std::iter::once(MatchSet {
            field_matches: &fields,
        }),
        |_| {},
    )
    .unwrap();
    let mut state = machine.allocate_state();

    let mut group = c.benchmark_group("match_string");
    for bloat in [0.0, 2.0, 8.0, 16.0, 32.0, 64.0] {
        let mut rng = StdRng::seed_from_u64(0xB00B5);
        let json = generate_test_json(&fields, 1.0, bloat, &mut rng);
        group.throughput(Throughput::Bytes(json.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("bloat_{bloat}")),
            &json,
            |b, json| {
                b.iter(|| {
                    machine.match_string(black_box(json), &mut state).unwrap();
                    black_box(&mut state);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_match);
criterion_main!(benches);
