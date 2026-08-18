mod cpu;

pub struct Emulator {
    cpu: cpu::CPU,
}

impl Emulator {
    pub fn new() -> Emulator {
        let cpu = cpu::CPU::new();

        Self { cpu: cpu }
    }
}
