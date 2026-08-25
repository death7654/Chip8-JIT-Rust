// jit packages
use dynasmrt::{DynasmApi, DynasmLabelApi, dynasm};
use std::io::{self, Read, Write};
use std::mem;
use std::os::raw::c_void;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READ, PAGE_READWRITE, VirtualAlloc, VirtualProtect,
};
const PAGE_SIZE: usize = 4096;
const CODE_ARENA_SIZE: usize = 64 * 1024;

// multi threaded programming
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

// emulator features
const HEIGHT: usize = 64;
const WIDTH: usize = 32;
const FONTSIZE: usize = 80;

/* Safety cap on how many CHIP-8 instructions we'll stitch into a single
   native block. Prevents a pathological/self-looping ROM (or a run of
   straight-line code that never hits a control-flow instruction) from
   producing an unbounded native function or exhausting the code arena
   in one compile() call.
*/
const MAX_BLOCK_INSTRUCTIONS: usize = 512;

pub struct TimerState {
    pub delay_timer: AtomicU8,
    pub sound_timer: AtomicU8,
    pub running: AtomicBool,
}

impl TimerState {
    pub fn new() -> Self {
        TimerState {
            delay_timer: AtomicU8::new(0),
            sound_timer: AtomicU8::new(0),
            running: AtomicBool::new(true),
        }
    }
}

#[repr(C)] // needed as our jit will interact with the register array
pub struct CPU {
    v: [u8; 16], // 16 B
    i: u16,      // 2 B
    pc: u16,     // 2 B

    pub stack: [u16; 16], // 32 B (Stores return PCs)
    pub sp: u8,           // 1 B  (Stack Pointer index 0..15)

    pub ram: [u8; 0x1000], // 4,096 B

    pub gfx: [u8; HEIGHT * WIDTH], // 2,048 B

    code_buffer: *mut u8,                   // Host JIT buffer ptr
    code_offset: usize,                     // Bump allocator index
    pub keys: [bool; 16],                   // keypad input
    instruction_cache: [*const u8; 0x1000], // 32,768 B
}

impl CPU {
    pub fn new(fonts: &[u8]) -> CPU {
        let code_buffer = unsafe {
            VirtualAlloc(
                std::ptr::null_mut(),
                CODE_ARENA_SIZE,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE, // start writable; flipped to RX before anything executes
            ) as *mut u8
        };

        assert!(
            !code_buffer.is_null(),
            "FATAL: Failed to allocate memory arena via VirtualAlloc"
        );

        let mut cpu = CPU {
            instruction_cache: [std::ptr::null_mut(); 0x1000],
            v: [0; 16],
            i: 0,
            pc: 0x200,
            stack: [0; 16],
            sp: 0,
            ram: [0; 0x1000],
            gfx: [0; HEIGHT * WIDTH],
            code_buffer,
            code_offset: 0,
            keys: [false; 16],
        };

        cpu.ram[0x050..0x050 + fonts.len()].copy_from_slice(fonts);
        cpu
    }

    fn compile(&mut self, start_pc: u16) -> *const u8 {
        let mut pc = start_pc;
        let mut ops = dynasmrt::x64::Assembler::new().unwrap();

        // Offsets for every CPU field relative to RCX (*mut CPU).
        // Constant for the whole block — computed once.
        let v_offset = std::mem::offset_of!(CPU, v) as i32;
        let i_offset = std::mem::offset_of!(CPU, i) as i32;
        let pc_offset = std::mem::offset_of!(CPU, pc) as i32;

        let stack_offset = std::mem::offset_of!(CPU, stack) as i32;
        let sp_offset = std::mem::offset_of!(CPU, sp) as i32;

        let delay_timer_offset = std::mem::offset_of!(TimerState, delay_timer) as i32;
        let sound_timer_offset = std::mem::offset_of!(TimerState, sound_timer) as i32;

        let ram_offset = std::mem::offset_of!(CPU, ram) as i32;
        let gfx_offset = std::mem::offset_of!(CPU, gfx) as i32;
        let keys_offset = std::mem::offset_of!(CPU, keys) as i32;

        let mut instr_count = 0usize;

        loop {
            if pc as usize + 1 >= 0x1000 {
                // Ran off the end of RAM: stop the block here.
                break;
            }

            let instruction =
                ((self.ram[pc as usize] as u16) << 8) | (self.ram[pc as usize + 1] as u16);

            let op = ((instruction >> 12) & 0x0F) as u8;
            let x = ((instruction >> 8) & 0x0F) as u8;
            let y = ((instruction >> 4) & 0x0F) as u8;
            let n = (instruction & 0x000F) as u8;
            let kk = (instruction & 0x00FF) as u8;
            let nnn = instruction & 0x0FFF;

            let vx_offset = v_offset + x as i32;
            let vy_offset = v_offset + y as i32;
            let vf_offset = v_offset + 15;

            // eprintln!(
            //     "  compile: pc={:#05x} opcode={:#06x} op={:#x} x={} y={} n={:#x} kk={:#04x} nnn={:#05x}",
            //     pc, instruction, op, x, y, n, kk, nnn
            // );

            let next_pc = pc.wrapping_add(2);
            dynasm!(ops
                ; .arch x64
                ; mov WORD [rcx + (pc_offset)], (next_pc as i16)
            );

            let is_terminator = match op {
                0x0 => match kk {
                    0xE0 => {
                        /* 00E0: Clear screen */
                        dynasm!(ops
                            ; .arch x64
                            ; lea r8, [rcx + gfx_offset]
                            ; xor rax, rax
                            ; mov r9d, 0
                            ; -> clear_loop:
                            ; mov [r8 + r9], rax
                            ; add r9, 8
                            ; cmp r9, (HEIGHT * WIDTH) as i32
                            ; jl -> clear_loop
                        );
                        false
                    }
                    0xEE => {
                        /* 00EE: Return from subroutine */
                        dynasm!(ops
                            ; .arch x64
                            ; dec BYTE [rcx + (sp_offset)]
                            ; movzx rax, BYTE [rcx + (sp_offset)]
                            ; mov dx, WORD [rcx + rax*2 + (stack_offset)]
                            ; mov WORD [rcx + (pc_offset)], dx
                            ; ret
                        );
                        true
                    }
                    _ => true, // Unknown 0NNN (SYS addr) — bail the block conservatively
                },
                0x1 => {
                    /* 1NNN: JP nnn */
                    dynasm!(ops
                        ; .arch x64
                        ; mov WORD [rcx + (pc_offset)], (nnn as i16)
                        ; ret
                    );
                    true
                }
                0x2 => {
                    /* 2NNN: CALL nnn */
                    let return_pc = next_pc as i16;
                    dynasm!(ops
                        ; .arch x64
                        ; movzx eax, BYTE [rcx + (sp_offset)]
                        ; mov WORD [rcx + rax*2 + (stack_offset)], return_pc
                        ; inc BYTE [rcx + (sp_offset)]
                        ; mov WORD [rcx + (pc_offset)], nnn as i16
                        ; ret
                    );
                    true
                }
                0x3 => {
                    /* 3XNN: SE Vx, kk */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vx_offset)]
                        ; cmp al, kk as i8
                        ; jne -> end
                        ; add WORD [rcx + (pc_offset)], 2
                        ; -> end:
                        ; ret
                    );
                    true
                }
                0x4 => {
                    /* 4XNN: SNE Vx, kk */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vx_offset)]
                        ; cmp al, kk as i8
                        ; je -> end
                        ; add WORD [rcx + (pc_offset)], 2
                        ; -> end:
                        ; ret
                    );
                    true
                }
                0x5 => {
                    /* 5XY0: SE Vx, Vy */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vx_offset)]
                        ; cmp al, BYTE [rcx + (vy_offset)]
                        ; jne -> end
                        ; add WORD [rcx + (pc_offset)], 2
                        ; -> end:
                        ; ret
                    );
                    true
                }
                0x6 => {
                    /* 6XNN: LD Vx, kk */
                    dynasm!(ops
                        ; .arch x64
                        ; mov BYTE [rcx + (vx_offset)], kk as i8
                    );
                    false
                }
                0x7 => {
                    /* 7XNN: ADD Vx, kk */
                    dynasm!(ops
                        ; .arch x64
                        ; add BYTE [rcx + (vx_offset)], kk as i8
                    );
                    false
                }
                0x8 => match n {
                    0x0 => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (vy_offset)]
                            ; mov BYTE [rcx + (vx_offset)], al
                        );
                        false
                    }
                    0x1 => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (vy_offset)]
                            ; or BYTE [rcx + (vx_offset)], al
                        );
                        false
                    }
                    0x2 => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (vy_offset)]
                            ; and BYTE [rcx + (vx_offset)], al
                        );
                        false
                    }
                    0x3 => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (vy_offset)]
                            ; xor BYTE [rcx + (vx_offset)], al
                        );
                        false
                    }
                    0x4 => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (vx_offset)]
                            ; mov dl, BYTE [rcx + (vy_offset)]
                            ; add al, dl
                            ; mov BYTE [rcx + (vx_offset)], al
                            ; setc al
                            ; mov BYTE [rcx + (vf_offset)], al
                        );
                        false
                    }
                    0x5 => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (vx_offset)]
                            ; mov dl, BYTE [rcx + (vy_offset)]
                            ; sub al, dl
                            ; mov BYTE [rcx + (vx_offset)], al
                            ; setnc al
                            ; mov BYTE [rcx + (vf_offset)], al
                        );
                        false
                    }
                    0x6 => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (vx_offset)]
                            ; shr al, 1
                            ; mov BYTE [rcx + (vx_offset)], al
                            ; setc al
                            ; mov BYTE [rcx + (vf_offset)], al
                        );
                        false
                    }
                    0x7 => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (vx_offset)]
                            ; mov dl, BYTE [rcx + (vy_offset)]
                            ; sub dl, al
                            ; mov BYTE [rcx + (vx_offset)], dl
                            ; setnc dl
                            ; mov BYTE [rcx + (vf_offset)], dl
                        );
                        false
                    }
                    0xE => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (vx_offset)]
                            ; shl al, 1
                            ; mov BYTE [rcx + (vx_offset)], al
                            ; setc al
                            ; mov BYTE [rcx + (vf_offset)], al
                        );
                        false
                    }
                    _ => true, // unimplemented 8XY_ — bail the block conservatively
                },
                0x9 => {
                    /* 9XY0: SNE Vx, Vy */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vx_offset)]
                        ; cmp al, BYTE [rcx + (vy_offset)]
                        ; je -> end
                        ; add WORD [rcx + (pc_offset)], 2
                        ; -> end:
                        ; ret
                    );
                    true
                }
                0xA => {
                    /* ANNN: LD I, nnn */
                    dynasm!(ops
                        ; .arch x64
                        ; mov WORD [rcx + (i_offset)], nnn as i16
                    );
                    false
                }
                0xB => {
                    /* BNNN: JP V0, nnn */
                    dynasm!(ops
                        ; .arch x64
                        ; movzx rax, BYTE [rcx + (v_offset)]
                        ; add rax, nnn as i32
                        ; mov WORD [rcx + (pc_offset)], ax
                        ; ret
                    );
                    true
                }
                0xC => {
                    /* CXNN: RND Vx, kk */
                    dynasm!(ops
                        ; .arch x64
                        ; rdrand rax
                        ; and al, kk as i8
                        ; mov BYTE [rcx + (vx_offset)], al
                    );
                    false
                }
                0xD => {
                    /* DXYN: DRW Vx, Vy, n */
                    let fn_ptr = draw_sprite as usize as i64;
                    dynasm!(ops
                        ; .arch x64
                        // Save RCX (*mut CPU pointer) on stack (-8 bytes)
                        ; push rcx
                        // Set up arguments for draw_sprite(cpu, vx, vy, n)
                        ; mov dl, BYTE [rcx + (vx_offset)]   // Arg 2: Vx (DL)
                        ; mov r8b, BYTE [rcx + (vy_offset)]  // Arg 3: Vy (R8B)
                        ; mov r9b, n as i8                   // Arg 4: N (R9B)
                        // Allocate 32 bytes of shadow space (RSP is 16-byte aligned here)
                        ; sub rsp, 32
                        ; mov rax, QWORD fn_ptr
                        ; call rax
                        // Clean up shadow space and restore RCX
                        ; add rsp, 32
                        ; pop rcx
                    );
                    false
                }
                0xE => match kk {
                    0x9E => {
                        dynasm!(ops
                            ; .arch x64
                            ; movzx rax, BYTE [rcx + (vx_offset)]
                            ; and rax, 0x0F
                            ; cmp BYTE [rcx + rax + (keys_offset)], 0
                            ; je -> end
                            ; add WORD [rcx + (pc_offset)], 2
                            ; -> end:
                            ; ret
                        );
                        true
                    }
                    0xA1 => {
                        dynasm!(ops
                            ; .arch x64
                            ; movzx rax, BYTE [rcx + (vx_offset)]
                            ; and rax, 0x0F
                            ; cmp BYTE [rcx + rax + (keys_offset)], 0
                            ; jne -> end
                            ; add WORD [rcx + (pc_offset)], 2
                            ; -> end:
                            ; ret
                        );
                        true
                    }
                    _ => true,
                },
                0xF => match kk {
                    0x07 => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (delay_timer_offset)]
                            ; mov BYTE [rcx + (vx_offset)], al
                        );
                        false
                    }
                    0x0A => {
                        // FX0A: LD Vx, K — waits for a keypress
                        dynasm!(ops
                            ; .arch x64
                            ; xor rax, rax
                            ; -> check_key:
                            ; cmp BYTE [rcx + rax + (keys_offset)], 0
                            ; jne -> key_pressed
                            ; inc rax
                            ; cmp rax, 16
                            ; jl -> check_key
                            ; sub WORD [rcx + (pc_offset)], 2
                            ; -> key_pressed:
                            ; mov BYTE [rcx + (vx_offset)], al
                            ; ret
                        );
                        true
                    }
                    0x15 => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (vx_offset)]
                            ; mov BYTE [rcx + (delay_timer_offset)], al
                        );
                        false
                    }
                    0x18 => {
                        dynasm!(ops
                            ; .arch x64
                            ; mov al, BYTE [rcx + (vx_offset)]
                            ; mov BYTE [rcx + (sound_timer_offset)], al
                        );
                        false
                    }
                    0x1E => {
                        dynasm!(ops
                            ; .arch x64
                            ; movzx ax, BYTE [rcx + (vx_offset)]
                            ; add WORD [rcx + (i_offset)], ax
                        );
                        false
                    }
                    0x29 => {
                        dynasm!(ops
                            ; .arch x64
                            ; movzx rax, BYTE [rcx + (vx_offset)]
                            ; and rax, 0x0F
                            ; imul ax, ax, 5
                            ; add ax, 0x50
                            ; mov WORD [rcx + (i_offset)], ax
                        );
                        false
                    }
                    0x33 => {
                        dynasm!(ops
                            ; .arch x64
                            ; movzx ax, BYTE [rcx + (vx_offset)]
                            ; movzx r8, WORD [rcx + (i_offset)]
                            ; and r8, 0xFFF
                            ; lea rdx, [rcx + r8 + (ram_offset)]
                            ; mov bl, 100
                            ; div bl
                            ; mov BYTE [rdx], al
                            ; mov al, ah
                            ; mov ah, 0
                            ; mov bl, 10
                            ; div bl
                            ; mov BYTE [rdx + 1], al
                            ; mov BYTE [rdx + 2], ah
                        );
                        false
                    }
                    0x55 => {
                        dynasm!(ops
                            ; .arch x64
                            ; movzx rax, WORD [rcx + (i_offset)]
                            ; and rax, 0xFFF
                        );
                        for reg in 0..=x {
                            let v_reg_offset = v_offset + reg as i32;
                            let offset = ram_offset + reg as i32;
                            dynasm!(ops
                                ; .arch x64
                                ; mov dl, BYTE [rcx + (v_reg_offset)]
                                ; mov BYTE [rcx + rax + (offset)], dl
                            );
                        }
                        false
                    }
                    0x65 => {
                        dynasm!(ops
                            ; .arch x64
                            ; movzx rax, WORD [rcx + (i_offset)]
                            ; and rax, 0xFFF
                        );
                        for reg in 0..=x {
                            let v_reg_offset = v_offset + reg as i32;
                            let offset = ram_offset + reg as i32;
                            dynasm!(ops
                                ; .arch x64
                                ; mov dl, BYTE [rcx + rax + (offset)]
                                ; mov BYTE [rcx + (v_reg_offset)], dl
                            );
                        }
                        false
                    }
                    _ => true, // unimplemented FX__ — bail the block conservatively
                },
                _ => true, // unreachable (op is 4 bits), kept for exhaustiveness
            };

            instr_count += 1;
            pc = next_pc;

            if is_terminator {
                break;
            }
            if instr_count >= MAX_BLOCK_INSTRUCTIONS {
                // Hit the safety cap on a straight-line run
                break;
            }
        }

        // ensure that a block ends
        dynasm!(ops; .arch x64; ret);

        let dest_ptr = unsafe { self.code_buffer.add(self.code_offset) };
        let exec_buffer: dynasmrt::ExecutableBuffer = ops.finalize().unwrap();
        let code_bytes: &[u8] = &exec_buffer;
        let len = code_bytes.len();

        assert!(
            self.code_offset + len <= CODE_ARENA_SIZE,
            "JIT code arena exhausted"
        );

        unsafe {
            let mut old_protect = 0u32;

            // Arena may currently be RX from a previous compile; make it writable again.
            VirtualProtect(
                self.code_buffer as *const c_void,
                CODE_ARENA_SIZE,
                PAGE_READWRITE,
                &mut old_protect,
            );

            std::ptr::copy_nonoverlapping(code_bytes.as_ptr(), dest_ptr, len);

            // Flip back to executable before anyone calls into it.
            VirtualProtect(
                self.code_buffer as *const c_void,
                CODE_ARENA_SIZE,
                PAGE_EXECUTE_READ,
                &mut old_protect,
            );
        }

        self.code_offset += len;
        self.instruction_cache[start_pc as usize] = dest_ptr;
        // eprintln!(
        //     "  block done: start_pc={:#05x} instr_count={} native_len={} dest_ptr={:?}",
        //     start_pc, instr_count, len, dest_ptr
        // );
        dest_ptr
    }

    pub fn execute(&mut self) {
        let pc = self.pc & 0x0FFE;

        let opcode = ((self.ram[pc as usize] as u16) << 8) | (self.ram[pc as usize + 1] as u16);
        // eprintln!("pc={:#05x} opcode={:#06x}", pc, opcode); // TEMP

        let func_ptr = if self.instruction_cache[pc as usize].is_null() {
            self.compile(pc)
        } else {
            self.instruction_cache[pc as usize]
        };

        unsafe {
            let func: extern "C" fn(*mut CPU) = mem::transmute(func_ptr);
            func(self as *mut CPU);
        }
    }
}

pub extern "C" fn draw_sprite(cpu: *mut CPU, vx: u8, vy: u8, n: u8) {
    let cpu = unsafe { &mut *cpu };
    let start_x = (vx as usize) % 64;
    let start_y = (vy as usize) % 32;

    // Reset collision flag VF
    cpu.v[0xF] = 0;

    for row in 0..(n as usize) {
        let y = (start_y + row) % 32;
        // Fetch sprite byte from RAM starting at address I
        let sprite_byte = cpu.ram[(cpu.i as usize + row) & 0xFFF];

        for col in 0..8 {
            let x = (start_x + col) % 64;
            // Check if bit (from MSB to LSB) is set
            let sprite_pixel = (sprite_byte >> (7 - col)) & 1;

            if sprite_pixel == 1 {
                let gfx_idx = y * 64 + x;
                // If screen pixel is already set, collision occurred
                if cpu.gfx[gfx_idx] == 1 {
                    cpu.v[0xF] = 1;
                }
                // XOR pixel onto screen
                cpu.gfx[gfx_idx] ^= 1;
            }
        }
    }
}
