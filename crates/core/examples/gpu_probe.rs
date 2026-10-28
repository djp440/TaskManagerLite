//! GPU 真实计数器冒烟：cargo run -p tml-core --example gpu_probe
fn main() {
    let mut sampler = tml_core::Sampler::default();
    for _ in 0..5 {
        let sample = sampler.sample_scope(tml_core::Scope::Metrics);
        println!("{:?}", sample.gpus);
        for gpu in sample.gpus {
            assert!(gpu.usage.is_none_or(|v| (0.0..=100.0).contains(&v)));
            assert!(gpu.dedicated.is_none_or(|v| v.is_finite() && v >= 0.0));
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
