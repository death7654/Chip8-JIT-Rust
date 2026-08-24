mod cpu;

pub struct Emulator {
    pub cpu: cpu::CPU,
}

impl Emulator {
    pub fn new() -> Emulator {
        let cpu = cpu::CPU::new();

        Self { cpu: cpu }
    }
    pub fn load(&mut self, data: &[u8]) {
        let start_address = 0x200 as usize;
        let end_address = &start_address + data.len();
        self.cpu.ram[start_address..end_address].copy_from_slice(data);
    }

    pub fn get_display(&self) -> &[u8; 2048] {
        &self.cpu.gfx
    }
}
