use sdl2::{
    EventPump,
    event::Event,
    keyboard::Keycode,
    render::{Canvas, Texture},
    video::Window,
};
use std::{
    sync::{Arc, atomic::Ordering},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod cpu;

// fonts
const FONTSIZE: usize = 80;
const FONT_SET: [u8; FONTSIZE] = [
    0xF0, 0x90, 0x90, 0x90, 0xF0, // 0
    0x20, 0x60, 0x20, 0x20, 0x70, // 1
    0xF0, 0x10, 0xF0, 0x80, 0xF0, // 2
    0xF0, 0x10, 0xF0, 0x10, 0xF0, // 3
    0x90, 0x90, 0xF0, 0x10, 0x10, // 4
    0xF0, 0x80, 0xF0, 0x10, 0xF0, // 5
    0xF0, 0x80, 0xF0, 0x90, 0xF0, // 6
    0xF0, 0x10, 0x20, 0x40, 0x40, // 7
    0xF0, 0x90, 0xF0, 0x90, 0xF0, // 8
    0xF0, 0x90, 0xF0, 0x10, 0xF0, // 9
    0xF0, 0x90, 0xF0, 0x90, 0x90, // A
    0xE0, 0x90, 0xE0, 0x90, 0xE0, // B
    0xF0, 0x80, 0x80, 0x80, 0xF0, // C
    0xE0, 0x90, 0x90, 0x90, 0xE0, // D
    0xF0, 0x80, 0xF0, 0x80, 0xF0, // E
    0xF0, 0x80, 0xF0, 0x80, 0x80, // F
];

pub struct Emulator {
    pub cpu: cpu::CPU,
    pub timer_handle: JoinHandle<()>,
}

impl Emulator {
    pub fn new() -> Emulator {
        let cpu = cpu::CPU::new(&FONT_SET);
        let timer_context = Arc::new(cpu::TimerState::new());
        let timer_handle: JoinHandle<()> = spawn_timer_thread(timer_context);
        Self {
            cpu: cpu,
            timer_handle,
        }
    }

    pub fn load(&mut self, data: &[u8]) {
        let start_address = 0x200 as usize;
        let end_address = &start_address + data.len();
        self.cpu.ram[start_address..end_address].copy_from_slice(data);
    }

    fn get_display(&self) -> &[u8; 2048] {
        &self.cpu.gfx
    }

    pub fn draw_screen(&mut self, canvas: &mut Canvas<Window>, texture: &mut Texture) {
        let screen_buf = self.get_display();

        // Lock memory slice of the texture and update pixel colors directly
        texture
            .with_lock(None, |pixel_buffer: &mut [u8], _pitch: usize| {
                for (i, &pixel) in screen_buf.iter().enumerate() {
                    let offset = i * 3; // 3 bytes per pixel (R, G, B)
                    let color = if pixel == 1 { 255 } else { 0 };

                    pixel_buffer[offset] = color; // Red
                    pixel_buffer[offset + 1] = color; // Green
                    pixel_buffer[offset + 2] = color; // Blue
                }
            })
            .unwrap();

        canvas.clear();
        // Copy the entire texture to full screen scaled automatically by SDL
        canvas.copy(texture, None, None).unwrap();
        canvas.present();
    }

    // helpers

    fn set_key(&mut self, key: usize, input: bool) {
        self.cpu.keys[key] = input;
    }

    pub fn check_inputs(&mut self, events: &mut EventPump) -> bool {
        let mut output = false;
        for evt in events.poll_iter() {
            match evt {
                Event::Quit { .. } => {
                    output = true;
                }
                Event::KeyDown {
                    keycode: Some(key), ..
                } => {
                    if let Some(key) = self.get_input(key) {
                        self.set_key(key, true);
                    } else {
                        println!("Invalid Input");
                    }
                }
                Event::KeyUp {
                    keycode: Some(key), ..
                } => {
                    if let Some(key) = self.get_input(key) {
                        self.set_key(key, false);
                    }
                }
                _ => (),
            }
        }

        output
    }

    // helpers
    fn get_input(&self, key: Keycode) -> Option<usize> {
        match key {
            Keycode::Num1 => Some(0x1),
            Keycode::Num2 => Some(0x2),
            Keycode::Num3 => Some(0x3),
            Keycode::Num4 => Some(0xC),
            Keycode::Q => Some(0x4),
            Keycode::W => Some(0x5),
            Keycode::E => Some(0x6),
            Keycode::R => Some(0xD),
            Keycode::A => Some(0x7),
            Keycode::S => Some(0x8),
            Keycode::D => Some(0x9),
            Keycode::F => Some(0xE),
            Keycode::Z => Some(0xA),
            Keycode::X => Some(0x0),
            Keycode::C => Some(0xB),
            Keycode::V => Some(0xF),
            _ => None,
        }
    }
}
fn spawn_timer_thread(ctx: Arc<cpu::TimerState>) -> JoinHandle<()> {
    thread::spawn(move || {
        let tick_interval = Duration::from_nanos(16_666_667); // 1.0 / 60.0 sec in nanoseconds
        let mut next_tick = Instant::now();

        while ctx.running.load(Ordering::Relaxed) {
            next_tick += tick_interval;

            // Decrement Delay Timer if > 0
            let dt = ctx.delay_timer.load(Ordering::Relaxed);
            if dt > 0 {
                ctx.delay_timer.store(dt - 1, Ordering::Relaxed);
            }

            // Decrement Sound Timer if > 0
            let st = ctx.sound_timer.load(Ordering::Relaxed);
            if st > 0 {
                ctx.sound_timer.store(st - 1, Ordering::Relaxed);
            }

            // Sleep until the exact next tick time slot
            let now = Instant::now();
            if next_tick > now {
                thread::sleep(next_tick - now);
            }
        }
    })
}
