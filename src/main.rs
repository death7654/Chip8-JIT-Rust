use std::{
    env::{self},
    fs::File,
    io::Read,
    time::{Duration, Instant}, 
};
mod Emulator;
use sdl2::{
    self, 
    event::Event, 
    keyboard::Keycode, 
    pixels::PixelFormatEnum, 
    render::{Canvas, Texture}, 
    video::Window
};

const HEIGHT: u32 = 32;
const WIDTH: u32 = 64;
const SCALE: u32 = 15;
const TICKS_PER_FRAME: usize = 10;

fn main() {
    // 1. Uncap VSync globally before initializing SDL video
    sdl2::hint::set("SDL_RENDER_VSYNC", "0");

    let mut emulator = Emulator::Emulator::new();
    let sdl_context = sdl2::init().unwrap();
    let video_subsystem = sdl_context.video().unwrap();

    let window = video_subsystem
        .window("Chip_8", WIDTH * SCALE, HEIGHT * SCALE)
        .position_centered()
        .opengl()
        .build()
        .unwrap();

    let mut canvas = window.into_canvas().build().unwrap();

    // 2. Create a 64x32 streaming texture (RGB24 format)
    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::RGB24, WIDTH, HEIGHT)
        .unwrap();

    let mut event_pump = sdl_context.event_pump().unwrap();
    let path = "roms/test_opcode.ch8";
    let mut rom = File::open(path).expect("Unable to open file");
    let mut buffer = Vec::new();

    rom.read_to_end(&mut buffer).unwrap();
    emulator.load(&buffer);

    // FPS tracking variables
    let mut last_time = Instant::now();
    let mut frame_count = 0;

    'gameloop: loop {
        for evt in event_pump.poll_iter() {
            match evt {
                Event::Quit { .. } => {
                    break 'gameloop;
                }
                Event::KeyDown {
                    keycode: Some(key), ..
                } => {
                    if let Some(key) = get_input(key) {
                        emulator.cpu.keys[key] = true;
                    } else {
                        println!("Invalid Input");
                    }
                }
                Event::KeyUp {
                    keycode: Some(key), ..
                } => {
                    if let Some(key) = get_input(key) {
                        emulator.cpu.keys[key] = false;
                    }
                }
                _ => (),
            }
        }

        if emulator.cpu.delay_timer > 0 {
            emulator.cpu.delay_timer -= 1;
        }

        for _ in 0..TICKS_PER_FRAME {
            emulator.cpu.execute();
        }

        // Pass texture into draw call
        draw_screen(&emulator, &mut canvas, &mut texture);

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
}

fn draw_screen(emu: &Emulator::Emulator, canvas: &mut Canvas<Window>, texture: &mut Texture) {
    let screen_buf = emu.get_display();

    // Lock memory slice of the texture and update pixel colors directly
    texture
        .with_lock(None, |pixel_buffer: &mut [u8], _pitch: usize| {
            for (i, &pixel) in screen_buf.iter().enumerate() {
                let offset = i * 3; // 3 bytes per pixel (R, G, B)
                let color = if pixel == 1 { 255 } else { 0 };

                pixel_buffer[offset] = color;     // Red
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

fn get_input(key: Keycode) -> Option<usize> {
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