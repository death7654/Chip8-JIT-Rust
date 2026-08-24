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

// emulator features
const HEIGHT: usize = 64;
const WIDTH: usize = 32;
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

#[repr(C)] // needed as our jit will interact with the register array
pub struct CPU {
    instruction_cache: [*const u8; 0x1000], // 32,768 B

    v: [u8; 16], // 16 B
    i: u16,      // 2 B
    pc: u16,     // 2 B

    pub stack: [u16; 16], // 32 B (Stores return PCs)
    pub sp: u8,           // 1 B  (Stack Pointer index 0..15)

    pub delay_timer: u8, // 1 B  (60Hz Delay Timer)
    pub sound_timer: u8, // 1 B  (60Hz Sound Timer)

    pub ram: [u8; 0x1000], // 4,096 B

    pub gfx: [u8; HEIGHT * WIDTH], // 2,048 B

    code_buffer: *mut u8, // Host JIT buffer ptr
    code_offset: usize,   // Bump allocator index

    pub keys: [bool; 16], // keypad input
}

impl CPU {
    pub fn new() -> CPU {
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
            delay_timer: 0,
            sound_timer: 0,
            ram: [0; 0x1000],
            gfx: [0; HEIGHT * WIDTH],
            code_buffer,
            code_offset: 0,
            keys: [false; 16],
        };

        cpu.ram[0x050..0x050 + FONT_SET.len()].copy_from_slice(&FONT_SET);
        cpu
    }

    fn compile(&mut self, instruction: u16, pc: u16) -> *const u8 {
        let op = ((instruction >> 12) & 0x0F) as u8;

        // standard instruction formats
        let x = ((instruction >> 8) & 0x0F) as u8;
        let y = ((instruction >> 4) & 0x0F) as u8;
        let n = (instruction & 0x000F) as u8;

        // special instruction formats
        let kk = (instruction & 0x00FF) as u8;
        let nnn = instruction & 0x0FFF;

        let mut compiled: Vec<u8> = Vec::new();

        // Offsets for every CPU field relative to RCX (*mut CPU)
        let v_offset = std::mem::offset_of!(CPU, v) as i32;
        let i_offset = std::mem::offset_of!(CPU, i) as i32;
        let pc_offset = std::mem::offset_of!(CPU, pc) as i32;

        // stack
        let stack_offset = std::mem::offset_of!(CPU, stack) as i32;
        let sp_offset = std::mem::offset_of!(CPU, sp) as i32;

        // sounds
        let delay_timer_offset = std::mem::offset_of!(CPU, delay_timer) as i32;
        let sound_timer_offset = std::mem::offset_of!(CPU, sound_timer) as i32;

        // ram
        let ram_offset = std::mem::offset_of!(CPU, ram) as i32;
        let gfx_offset = std::mem::offset_of!(CPU, gfx) as i32;
        let keys_offset = std::mem::offset_of!(CPU, keys) as i32;

        // jit asm
        let mut ops = dynasmrt::x64::Assembler::new().unwrap();

        // additional offsets for ease of usage
        let vx_offset = v_offset + x as i32;
        let vy_offset = v_offset + y as i32;
        let vf_offset = v_offset + 15 as i32;

        match op {
            0x0 => match kk {
                0xE0 => {
                    /* Clear screen */
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
                        ; ret
                    )
                }
                0xEE => {
                    /* 00EE: Return from subroutine */
                    dynasm!(ops
                        ; .arch x64
                        // Decrement SP
                        ; dec BYTE [rcx + (sp_offset)]

                        // Zero-extend SP into RAX to use as an array index
                        ; movzx rax, BYTE [rcx + (sp_offset)]

                        // Load return address from stack[sp] (scaled by 2 bytes per u16)
                        ; mov dx, WORD [rcx + rax*2 + (stack_offset)]

                        // Update PC with the return address
                        ; mov WORD [rcx + (pc_offset)], dx
                        ; ret
                    )
                }
                _ => {}
            },
            0x1 => {
                /* 1NNN: JP nnn - Jump to address NNN */
                dynasm!(ops
                    ; .arch x64
                    ; mov WORD [rcx + (pc_offset as i32)], (nnn as i16)
                    ; ret
                );
            }
            0x2 => {
                /* 2NNN: CALL nnn - Call subroutine at NNN */
                let return_pc = (pc + 2) as i16;
                dynasm!(ops
                    ; .arch x64

                    // Zero-extend current SP into RAX to use as array index
                    ; movzx eax, BYTE [rcx + (sp_offset as i32)]

                    // Save return address (PC + 2) into stack[SP]
                    ; mov WORD [rcx + rax*2 + (stack_offset)], return_pc

                    // Increment SP (ready for next call)
                    ; inc BYTE [rcx + (sp_offset)]

                    // Jump to subroutine address NNN
                    ; mov WORD [rcx + (pc_offset)], nnn as i16
                    ; ret
                );
            }
            0x3 => {
                /* 3XNN: SE Vx, kk - Skip next instruction if Vx == KK */
                dynasm!(ops
                    ; .arch x64
                    ; mov al, BYTE [rcx + (vx_offset)]
                    ; cmp al, kk as i8
                    ; jne -> end
                    ; add WORD [rcx + (pc_offset)], 2 // Extra +2 skip
                    ; -> end:
                    ; ret
                );
            }
            0x4 => {
                /* 4XNN: SNE Vx, kk - Skip next instruction if Vx != KK */
                dynasm!(ops
                ; .arch x64
                ; mov al, BYTE [rcx + (vx_offset)]
                ; cmp al, kk as i8
                ; je -> end
                ; add WORD [rcx + (pc_offset)], 2 // Extra +2 skip
                ; -> end:
                ; ret
                )
            }
            0x5 => {
                /* 5XY0: SE Vx, Vy - Skip next instruction if Vx == Vy */
                dynasm!(ops
                    ; .arch x64
                    ; mov al, BYTE [rcx + (vx_offset)]
                    ; cmp al, BYTE [rcx + (vy_offset)]
                    ; jne -> end
                    ; add WORD [rcx + (pc_offset)], 2
                    ; -> end:
                    ; ret
                );
            }
            0x6 => {
                /* 6XNN: LD Vx, kk - Set Vx = KK */
                dynasm!(ops
                    ; .arch x64
                    ; mov BYTE [rcx + (vx_offset)], kk as i8
                    ; ret
                );
            }
            0x7 => {
                /* 7XNN: ADD Vx, kk - Set Vx = Vx + KK */
                dynasm!(ops
                    ; .arch x64
                    ; add BYTE [rcx + (vx_offset)], kk as i8
                    ; ret
                );
            }
            0x8 => match n {
                0x0 => {
                    /* 8XY0: LD Vx, Vy - Set Vx = Vy */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vy_offset)]
                        ; mov BYTE [rcx + (vx_offset)], al
                        ; ret
                    );
                }
                0x1 => {
                    /* 8XY1: OR Vx, Vy - Set Vx = Vx | Vy */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vy_offset)]
                        ; or BYTE [rcx + (vx_offset)], al
                        ; ret
                    )
                }
                0x2 => {
                    /* 8XY2: AND Vx, Vy - Set Vx = Vx & Vy */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vy_offset)]
                        ; and BYTE [rcx + (vx_offset)], al
                        ; ret
                    )
                }
                0x3 => {
                    /* 8XY3: XOR Vx, Vy - Set Vx = Vx ^ Vy */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vy_offset)]
                        ; xor BYTE [rcx + (vx_offset)], al
                        ; ret
                    )
                }
                0x4 => {
                    /* 8XY4: ADD Vx, Vy - Set Vx = Vx + Vy, VF = carry */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vx_offset)]  // Load Vx into AL
                        ; mov dl, BYTE [rcx + (vy_offset)]  // Load Vy into DL
                        ; add al, dl                            // AL = AL + DL (Updates x86 Carry Flag)
                        ; mov BYTE [rcx + (vx_offset)], al  // Store 8-bit result in Vx
                        ; setc al                               // AL = 1 if CF==1 else 0
                        ; mov BYTE [rcx + (vf_offset)], al  // Store carry flag in VF
                        ; ret
                    );
                }
                0x5 => {
                    /* 8XY5: SUB Vx, Vy - Set Vx = Vx - Vy, VF = NOT borrow */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vx_offset)]  // Load Vx into AL
                        ; mov dl, BYTE [rcx + (vy_offset)]  // Load Vy into DL
                        ; sub al, dl                            // AL = AL - DL (Sets x86 Carry/Borrow Flag)
                        ; mov BYTE [rcx + (vx_offset)], al  // Store 8-bit result in Vx
                        ; setnc al                              // AL = 1 if CF==0 (Vx >= Vy), AL = 0 if CF==1 (Vx < Vy)
                        ; mov BYTE [rcx + (vf_offset)], al  // Store NOT borrow flag in VF
                        ; ret
                    );
                }
                0x6 => {
                    /* 8XY6: SHR Vx - Set Vx = Vx >> 1 */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vx_offset)]  // Load Vx into AL
                        ; shr al, 1                             // AL = AL >> 1
                        ; mov BYTE [rcx + (vx_offset)], al  // Store shifted value in Vx
                        ; setc al                               // Copy Carry Flag (the LSB) into AL
                        ; mov BYTE [rcx + (vf_offset)], al  // Store LSB in VF
                        ; ret
                    );
                }
                0x7 => {
                    /* 8XY7: SUBN Vx, Vy - Set Vx = Vy - Vx, VF = NOT borrow */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vx_offset)]  // AL = Vx
                        ; mov dl, BYTE [rcx + (vy_offset)]  // DL = Vy
                        ; sub dl, al                            // DL = Vy - Vx (Updates x86 Carry/Borrow Flag)
                        ; mov BYTE [rcx + (vx_offset)], dl  // Store result (DL) into Vx
                        ; setnc dl                              // DL = 1 if CF==0 (Vy >= Vx), 0 if CF==1 (Vy < Vx)
                        ; mov BYTE [rcx + (vf_offset)], dl  // Store NOT borrow flag (DL) into VF
                        ; ret
                    );
                }
                0xE => {
                    /* 8XYE: SHL Vx - Set Vx = Vx << 1 */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vx_offset)]  // Load Vx into AL
                        ; shl al, 1                             // AL = AL << 1 (Old MSB moves into x86 Carry Flag)
                        ; mov BYTE [rcx + (vx_offset)], al  // Store shifted value in Vx
                        ; setc al                               // Copy Carry Flag (the MSB) into AL
                        ; mov BYTE [rcx + (vf_offset)], al  // Store MSB in VF
                        ; ret
                    );
                }
                _ => {}
            },
            0x9 => {
                /* 9XY0: SNE Vx, Vy - Skip next instruction if Vx != Vy */
                dynasm!(ops
                    ; .arch x64
                    ; mov al, BYTE [rcx + (vx_offset)]
                    ; cmp al, BYTE [rcx + (vy_offset)]
                    ; je -> end
                    ; add WORD [rcx + (pc_offset)], 2
                    ; -> end:
                    ; ret
                );
            }
            0xA => {
                /* ANNN: LD I, nnn - Set I = NNN */
                dynasm!(ops
                    ; .arch x64
                    ; mov WORD [rcx + (i_offset)], nnn as i16
                    ; ret
                );
            }
            0xB => {
                /* BNNN: JP V0, nnn - Jump to address NNN + V0 */
                dynasm!(ops
                    ; .arch x64
                    ; movzx rax, BYTE [rcx + (v_offset)]
                    ; add rax, nnn as i32
                    ; mov WORD [rcx + (pc_offset)], ax
                    ; ret
                )
            }
            0xC => {
                /* CXNN: RND Vx, kk - Set Vx = random byte AND KK */
                dynasm!(ops
                    ; .arch x64
                    ; rdrand rax                            // CPU fills EAX with hardware random bits
                    ; and al, kk as i8                     // AL = random_byte & NN
                    ; mov BYTE [rcx + (vx_offset)], al  // Store result in Vx
                    ; ret
                );
            }
            0xD => {
                /* DXYN: DRW Vx, Vy, n - Draw N-byte sprite at (Vx, Vy) */
                let fn_ptr = draw_sprite as usize as i64;
                let vx_offset = v_offset + x as i32;
                let vy_offset = v_offset + y as i32;

                dynasm!(ops
                    ; .arch x64
                    // Pass Arguments via Windows x64 ABI
                    // RCX is already pointing to *mut CPU (Arg 1)
                    ; mov dl, BYTE [rcx + (vx_offset)]   // Arg 2: Vx (DL)
                    ; mov r8b, BYTE [rcx + (vy_offset)]  // Arg 3: Vy (R8B)
                    ; mov r9b, n as i8                      // Arg 4: N (R9B)

                    // Allocate 32-byte shadow space required by Windows ABI
                    ; sub rsp, 40

                    // Call Rust helper function
                    ; mov rax, QWORD fn_ptr // qword = 32 bit
                    ; call rax

                    // Clean up stack frame
                    ; add rsp, 40
                    ; ret
                );
            }
            0xE => match kk {
                0x9E => {
                    /* EX9E: SKP Vx - Skip next instruction if key in Vx is pressed */
                    dynasm!(ops
                        ; .arch x64
                        ; movzx rax, BYTE [rcx + (vx_offset)]
                        ; and rax, 0x0F
                        ; cmp BYTE [rcx + rax + (keys_offset)], 0
                        ; je -> end                                   // If NOT pressed (0), don't skip
                        ; add WORD [rcx + (pc_offset)], 2         // Pressed: advance PC by +2 (skip)
                        ; -> end:
                        ; ret
                    )
                }
                0xA1 => {
                    /* EXA1: SKNP Vx - Skip next instruction if key in Vx is NOT pressed */
                    dynasm!(ops
                        ; .arch x64
                        ; movzx rax, BYTE [rcx + (vx_offset)]
                        ; and rax, 0x0F
                        ; cmp BYTE [rcx + rax + (keys_offset)], 0
                        ; jne -> end
                        ; add WORD [rcx + (pc_offset)], 2
                        ; -> end:
                        ; ret
                    )
                }
                _ => {}
            },
            0xF => match kk {
                0x07 => {
                    /* FX07: LD Vx, DT - Set Vx = delay timer value */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (delay_timer_offset)]
                        ; mov BYTE [rcx + (vx_offset)], al
                        ; ret
                    )
                }
                0x0A => {
                    /* FX0A: LD Vx, K - Wait for key press, store key in Vx */
                    dynasm!(ops
                        ; .arch x64
                        ; xor rax, rax                              // RAX = 0 (key index counter)

                        ; -> check_key:
                        ; cmp BYTE [rcx + rax + (keys_offset)], 0 // Is keys[RAX] pressed?
                        ; jne -> key_pressed                        // Found a pressed key! Jump out.
                        ; inc rax                                   // RAX++
                        ; cmp rax, 16                               // Checked all 16 keys?
                        ; jl -> check_key                           // Loop if RAX < 16

                        //  No key was pressed
                        ; sub WORD [rcx + (pc_offset)], 2       // Rewind PC to re-execute FX0A next frame
                        ; ret

                        //  Key was pressed
                        ; -> key_pressed:
                        ; mov BYTE [rcx + (vx_offset)], al      // Store key index (AL) into Vx
                        ; ret
                    );
                }
                0x15 => {
                    /* FX15: LD DT, Vx - Set delay timer = Vx */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vx_offset)]
                        ; mov BYTE [rcx + (delay_timer_offset)], al
                        ; ret
                    )
                }
                0x18 => {
                    /* FX18: LD ST, Vx - Set sound timer = Vx */
                    dynasm!(ops
                        ; .arch x64
                        ; mov al, BYTE [rcx + (vx_offset)]
                        ; mov BYTE [rcx + (sound_timer_offset)], al
                        ; ret
                    )
                }
                0x1E => {
                    /* FX1E: ADD I, Vx - Set I = I + Vx */
                    dynasm!(ops
                        ; .arch x64
                        ; movzx ax, BYTE [rcx + (vx_offset)]
                        ; add WORD[rcx + (i_offset)], ax
                        ; ret
                    )
                }
                0x29 => {
                    /* FX29: LD F, Vx - Set I = location of sprite for digit Vx */
                    dynasm!(ops
                        ; .arch x64
                        ; movzx rax, BYTE [rcx + (vx_offset)]
                        ; and rax, 0x0F
                        ; imul ax, ax, 5
                        ; add ax, 0x50
                        ; mov WORD [rcx + (i_offset)], ax
                        ; ret
                    );
                }
                0x33 => {
                    /* FX33: LD B, Vx - Store BCD representation of Vx in I, I+1, I+2 */
                    dynasm!(ops
                        ; .arch x64
                        ; movzx ax, BYTE [rcx + (vx_offset)]      // AX = Vx (0..255)
                        ; movzx r8, WORD [rcx + (i_offset)]       // R8 = I
                        ; and r8, 0xFFF                               // Mask address to 4KB RAM bounds
                        ; lea rdx, [rcx + r8 + (ram_offset)]          // RDX = &ram[I]

                        // --- Hundreds Digit ---
                        ; mov bl, 100
                        ; div bl                                      // AL = AX / 100 (Hundreds), AH = AX % 100
                        ; mov BYTE [rdx], al                      // ram[I] = Hundreds

                        // --- Tens & Ones Digits ---
                        ; mov al, ah                                  // AL = remainder (0..99)
                        ; mov ah, 0                                   // Clear AH so AX = remainder
                        ; mov bl, 10
                        ; div bl                                      // AL = AX / 10 (Tens), AH = AX % 10 (Ones)
                        ; mov BYTE [rdx + 1], al                  // ram[I+1] = Tens
                        ; mov BYTE [rdx + 2], ah                  // ram[I+2] = Ones
                        ; ret
                    );
                }
                0x55 => {
                    /* FX55: LD [I], Vx - Store registers V0 through Vx in memory starting at I */

                    dynasm!(ops
                        ; .arch x64
                        ; movzx rax, WORD [rcx + (i_offset)]       // RAX = I
                        ; and rax, 0xFFF                               // Mask address to 4KB RAM bounds
                    );

                    // Unroll instruction emission for V0..=Vx
                    for reg in 0..=x {
                        let v_reg_offset = v_offset + reg as i32;
                        let offset = ram_offset + reg as i32;
                        dynasm!(ops
                            ; .arch x64
                            ; mov dl, BYTE [rcx + (v_reg_offset)] // Load V[reg] into DL
                            ; mov BYTE [rcx + rax + (offset)], dl // Store into ram[I + reg]
                        );
                    }

                    dynasm!(ops
                        ; .arch x64
                        ; ret
                    );
                }
                0x65 => {
                    /* FX65: LD Vx, [I] - Read memory starting at I into registers V0 through Vx */

                    dynasm!(ops
                        ; .arch x64
                        ; movzx rax, WORD [rcx + (i_offset)]       // RAX = I
                        ; and rax, 0xFFF                               // Mask address to 4KB RAM bounds
                    );

                    // Unroll instruction emission for V0..=Vx
                    for reg in 0..=x {
                        let v_reg_offset = v_offset + reg as i32;
                        let offset = ram_offset + reg as i32;
                        dynasm!(ops
                            ; .arch x64
                            ; mov dl, BYTE [rcx + rax + (offset)] // Load byte from ram[I + reg]
                            ; mov BYTE [rcx + (v_reg_offset)], dl // Store into V[reg]
                        );
                    }

                    dynasm!(ops
                        ; .arch x64
                        ; ret
                    );
                }
                _ => {}
            },
            _ => {}
        }

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
        self.instruction_cache[pc as usize] = dest_ptr;
        dest_ptr
    }
    pub fn execute(&mut self) {
        let pc = self.pc & 0x0FFE;
        self.pc = pc.wrapping_add(2);

        let opcode = ((self.ram[pc as usize] as u16) << 8) | (self.ram[pc as usize + 1] as u16);
        // eprintln!("pc={:#05x} opcode={:#06x}", pc, opcode); // TEMP


        let func_ptr = if self.instruction_cache[pc as usize].is_null() {
            self.compile(opcode, pc)
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
