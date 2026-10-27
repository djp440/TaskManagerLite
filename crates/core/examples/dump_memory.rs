//! 输出两次采样得到的内存状态与硬件信息（每行 `键=值`，字节为单位），供 scripts/verify-memory.ps1 与系统计数器、WMI 对比。

use tml_core::Sampler;

fn main() {
    let mut sampler = Sampler::default();
    sampler.sample();
    let sample = sampler.sample();
    let m = sample.memory;
    let hw = sample.memory_hardware.unwrap_or_default();
    println!("total={}", m.total);
    println!("available={}", m.available);
    println!("free={}", m.free);
    println!("modified={}", m.modified);
    println!("standby={}", m.standby);
    println!("compressed={}", m.compressed);
    println!("paged_pool={}", m.paged_pool);
    println!("nonpaged_pool={}", m.nonpaged_pool);
    println!("committed={}", m.committed);
    println!("commit_limit={}", m.commit_limit);
    println!("installed={}", hw.installed);
    println!("speed_mts={}", hw.speed_mts.unwrap_or(0));
    println!("slots_used={}", hw.slots_used);
    println!("slots_total={}", hw.slots_total);
    println!("form_factor={}", hw.form_factor.unwrap_or(""));
    println!("memory_type={}", hw.memory_type.unwrap_or(""));
}
