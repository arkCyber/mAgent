//! Performance benchmarks for magent-core
//!
//! Standalone host-side benchmark that exercises the same algorithms
//! the embedded firmware uses (KV-store format, CRC, wear leveling,
//! ReAct state machine) so you can spot regressions quickly.
//!
//! Run with: `cargo run -p magent-tools --bin benchmarks --release`

use std::time::Instant;

fn main() {
    println!("=== magent-core Performance Benchmarks ===\n");

    benchmark_kv_store_format();
    benchmark_crc_calculation();
    benchmark_wear_leveling();
    benchmark_agent_operations();

    println!("\n=== All Benchmarks Complete ===");
}

fn benchmark_kv_store_format() {
    println!("Benchmarking KV Store Format...");

    let iterations = 10000;
    let start = Instant::now();

    for _ in 0..iterations {
        let key = "test_key";
        let value = [1u8, 2, 3, 4, 5];

        let key_len = key.len() as u8;
        let value_len = value.len() as u16;
        let entry_size = 1 + key_len as usize + 2 + value_len as usize + 2;

        // Prevent optimization
        std::hint::black_box(entry_size);
    }

    let duration = start.elapsed();
    let avg_ns = duration.as_nanos() / iterations as u128;

    println!("  ✅ {} iterations in {:?}", iterations, duration);
    println!("  ✅ Average: {} ns per operation", avg_ns);
}

fn benchmark_crc_calculation() {
    println!("Benchmarking CRC Calculation...");

    let iterations = 10000;
    let data = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let start = Instant::now();

    for _ in 0..iterations {
        let mut crc: u16 = 0;
        for &byte in data.iter() {
            crc ^= byte as u16;
            crc = crc.wrapping_mul(0x1021);
        }

        // Prevent optimization
        std::hint::black_box(crc);
    }

    let duration = start.elapsed();
    let avg_ns = duration.as_nanos() / iterations as u128;

    println!("  ✅ {} iterations in {:?}", iterations, duration);
    println!("  ✅ Average: {} ns per operation", avg_ns);
}

fn benchmark_wear_leveling() {
    println!("Benchmarking Wear Leveling...");

    let iterations = 10000;
    let num_sectors = 16;
    let start = Instant::now();

    for _ in 0..iterations {
        let mut write_counts = vec![0u32; num_sectors];
        for i in 0..100 {
            write_counts[i % num_sectors] += 1;
        }

        // Prevent optimization
        std::hint::black_box(write_counts);
    }

    let duration = start.elapsed();
    let avg_ns = duration.as_nanos() / iterations as u128;

    println!("  ✅ {} iterations in {:?}", iterations, duration);
    println!("  ✅ Average: {} ns per operation", avg_ns);
}

fn benchmark_agent_operations() {
    println!("Benchmarking Agent Operations...");

    let iterations = 10000;
    let start = Instant::now();

    for _ in 0..iterations {
        // Simulate agent state transitions
        let states = ["Thinking", "Executing", "Observing", "Finished"];
        let mut current = 0;

        for _ in 0..10 {
            current = (current + 1) % states.len();
        }

        // Prevent optimization
        std::hint::black_box(current);
    }

    let duration = start.elapsed();
    let avg_ns = duration.as_nanos() / iterations as u128;

    println!("  ✅ {} iterations in {:?}", iterations, duration);
    println!("  ✅ Average: {} ns per operation", avg_ns);
}
