use sdl2::{self, pixels::PixelFormatEnum};
use std::{
    fs::File,
    io::Read,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

const HEIGHT: u32 = 32;
const WIDTH: u32 = 64;
const SCALE: u32 = 15;

mod emulator;

fn main() {
    // Uncap VSync globally before initializing SDL video
    sdl2::hint::set("SDL_RENDER_VSYNC", "0");

    let sdl_context = sdl2::init().unwrap();
    let video_subsystem = sdl_context.video().unwrap();

    let window = video_subsystem
        .window("Chip_8", WIDTH * SCALE, HEIGHT * SCALE)
        .position_centered()
        .opengl()
        .build()
        .unwrap();

    let mut canvas = window.into_canvas().build().unwrap();

    // Create a 64x32 streaming texture
    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::RGB24, WIDTH, HEIGHT)
        .unwrap();
    let mut event_pump = sdl_context.event_pump().unwrap();

    // read file
    let path = "roms/test_opcode.ch8";
    let mut rom = File::open(path).expect("Unable to open file");
    let mut buffer = Vec::new();

    rom.read_to_end(&mut buffer).unwrap();

    let mut emulator = emulator::Emulator::new();
    emulator.load(&buffer);

    // FPS tracking variables
    let mut last_time = Instant::now();
    let mut frame_count = 0;

    'gameloop: loop {
        if emulator.check_inputs(&mut event_pump) {
            break 'gameloop;
        }
        emulator.cpu.execute();

        // Pass texture into draw call
        emulator.draw_screen(&mut canvas, &mut texture);

        // Update FPS counter
        frame_count += 1;
        if last_time.elapsed() >= Duration::from_secs(1) {
            let fps = frame_count as f64 / last_time.elapsed().as_secs_f64();
            canvas
                .window_mut()
                .set_title(&format!("Chip_8 | FPS: {:.0}", fps))
                .ok();
            frame_count = 0;
            last_time = Instant::now();
        }
    }

    emulator.timer_handle.join().unwrap();
}
