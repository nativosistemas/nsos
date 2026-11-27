//! GDT y TSS.
//!
//! En modo largo la segmentación casi no se usa, pero hacen falta: un
//! descriptor de código de kernel, los de usuario (para `sysret`, hito 4) y
//! el TSS, que da pilas IST para excepciones críticas (doble fallo) y la
//! pila de anillo 0 al entrar desde una cell.
//!
//! # Invariantes (`unsafe`)
//! `init` se llama una vez, en el BSP, con interrupciones deshabilitadas. La
//! GDT y el TSS son `static` y viven para siempre, como exige la CPU.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;

/// Selector de código de kernel.
pub const KERNEL_CS: u16 = 0x08;
/// Selector de datos de kernel.
pub const KERNEL_DS: u16 = 0x10;
/// Selector de datos de usuario (RPL 3).
pub const USER_DS: u16 = 0x18 | 3;
/// Selector de código de usuario (RPL 3).
pub const USER_CS: u16 = 0x20 | 3;
const TSS_SEL: u16 = 0x28;

/// Índice IST (1-based) usado por el doble fallo.
pub const IST_DOUBLE_FAULT: u8 = 1;
const IST_STACK_SIZE: usize = 16 * 1024;

#[repr(C, packed(4))]
struct Tss {
    _r0: u32,
    rsp: [u64; 3],
    _r1: u64,
    ist: [u64; 7],
    _r2: u64,
    _r3: u16,
    iomap_base: u16,
}

#[repr(C, align(16))]
struct Stack([u8; IST_STACK_SIZE]);

struct Statics {
    gdt: UnsafeCell<[u64; 7]>,
    tss: UnsafeCell<Tss>,
    df_stack: UnsafeCell<Stack>,
}

// SAFETY: solo `init` (BSP, una vez, sin interrupciones) escribe; después la
// CPU es la única que lee estas estructuras.
unsafe impl Sync for Statics {}

// En `.kpti_data`: la CPU lee la GDT y la TSS (y usa la pila del doble
// fallo) también con las tablas de usuario de KPTI.
#[link_section = ".kpti_data"]
static S: Statics = Statics {
    gdt: UnsafeCell::new([
        0,
        0x00af_9a00_0000_ffff, // código kernel 64 bits, DPL 0
        0x00cf_9200_0000_ffff, // datos kernel, DPL 0
        0x00cf_f200_0000_ffff, // datos usuario, DPL 3
        0x00af_fa00_0000_ffff, // código usuario 64 bits, DPL 3
        0,                     // TSS (parte baja), se rellena en init
        0,                     // TSS (parte alta)
    ]),
    tss: UnsafeCell::new(Tss {
        _r0: 0,
        rsp: [0; 3],
        _r1: 0,
        ist: [0; 7],
        _r2: 0,
        _r3: 0,
        iomap_base: size_of::<Tss>() as u16, // sin bitmap de E/S
    }),
    df_stack: UnsafeCell::new(Stack([0; IST_STACK_SIZE])),
};

#[repr(C, packed)]
struct DescriptorPtr {
    limit: u16,
    base: u64,
}

/// Carga la GDT, recarga los segmentos y el registro de tarea.
pub fn init() {
    // SAFETY: ver invariantes del módulo.
    unsafe {
        let tss = &mut *S.tss.get();
        let df_top = (S.df_stack.get() as u64) + IST_STACK_SIZE as u64;
        tss.ist[(IST_DOUBLE_FAULT - 1) as usize] = df_top;
        // KPTI: desde anillo 3, la CPU deja el marco en la pila de entrada
        // (la única pila que ven las tablas de usuario).
        core::ptr::addr_of_mut!(tss.rsp).write_unaligned([super::kpti::entry_stack_top(), 0, 0]);

        let base = S.tss.get() as u64;
        let limit = (size_of::<Tss>() - 1) as u64;
        let low = (limit & 0xffff)
            | ((base & 0xff_ffff) << 16)
            | (0x89 << 40) // presente, TSS de 64 bits disponible
            | (((limit >> 16) & 0xf) << 48)
            | (((base >> 24) & 0xff) << 56);
        let gdt = &mut *S.gdt.get();
        gdt[5] = low;
        gdt[6] = base >> 32;

        let ptr = DescriptorPtr {
            limit: (size_of::<[u64; 7]>() - 1) as u16,
            base: gdt.as_ptr() as u64,
        };
        asm!(
            "lgdt [{ptr}]",
            // Recargar CS con un far return.
            "push {cs}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {ds:x}",
            "mov es, {ds:x}",
            "mov ss, {ds:x}",
            "xor {tmp:e}, {tmp:e}",
            "mov fs, {tmp:x}",
            "mov gs, {tmp:x}",
            "ltr {tss:x}",
            ptr = in(reg) &ptr,
            cs = in(reg) KERNEL_CS as u64,
            ds = in(reg) KERNEL_DS as u64,
            tss = in(reg) TSS_SEL as u64,
            tmp = out(reg) _,
        );
    }
}
