//! 普通权限运行，核对实际拓扑与两次 PDH 采样。
fn main() {
    let mut sampler = tml_core::Sampler::default();
    for _ in 0..3 {
        let sample = sampler.sample_scope(tml_core::Scope::Metrics);
        println!("{:#?}", sample.disks);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
