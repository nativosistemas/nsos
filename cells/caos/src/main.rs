//! Cell `caos`: fuzzer de syscalls dentro de QEMU.
//!
//! Arranca sin capacidades y hace `ROUNDS` syscalls con números y
//! argumentos pseudoaleatorios. La semilla sale de `CAOS_SEED` (un número
//! decimal) al compilar, y si no está, es fija. La imprime al empezar: para
//! repetir una corrida, `CAOS_SEED=<semilla> make smoke`. La CI usa una
//! semilla distinta en cada corrida:
//! handles inventados o propios, punteros al kernel, a su código, a su pila
//! o sin mapear, tamaños y flags arbitrarios. Lo único que se acota es lo
//! que la dejaría bloqueada para siempre (las esperas van con `NONBLOCK` o
//! plazo 0, también `channel_call`) o que podría quitarle memoria a las demás cells (objetos de memoria
//! chicos). Nunca llama a `exit` al azar.
//!
//! El kernel tiene que devolver un error o un resultado válido en cada
//! llamada y seguir funcionando. Al final imprime cuántas veces vio cada
//! resultado y termina con 0.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::syscall::{flags::NONBLOCK, Syscall};
use nsos_rt::abi::Error;
use nsos_rt::{println, raw_syscall};

const ROUNDS: u32 = 50_000;
/// Semilla por omisión: "nsos-cao".
const DEFAULT_SEED: u64 = 0x6e73_6f73_2d63_616f;
/// Semilla de esta compilación (ver el comentario del módulo).
const SEED: u64 = match option_env!("CAOS_SEED") {
    Some(s) => parse_seed(s.as_bytes()),
    None => DEFAULT_SEED,
};

/// Lee un número decimal. Si no lo es, no compila. xorshift no puede
/// empezar en 0, así que el 0 se cambia por la semilla por omisión.
const fn parse_seed(b: &[u8]) -> u64 {
    assert!(!b.is_empty(), "CAOS_SEED vacía");
    let mut n: u64 = 0;
    let mut i = 0;
    while i < b.len() {
        assert!(
            b[i].is_ascii_digit(),
            "CAOS_SEED tiene que ser un número decimal"
        );
        n = n.wrapping_mul(10).wrapping_add((b[i] - b'0') as u64);
        i += 1;
    }
    if n == 0 {
        DEFAULT_SEED
    } else {
        n
    }
}
const KERNEL_ADDR: u64 = 0xffff_ffff_8000_0000;

nsos_rt::entry!(main);

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Syscalls existentes salvo `exit`, más algunos números inválidos.
const CALLS: [u64; 23] = [
    0, 1, 3, 10, 11, 12, 20, 21, 22, 23, 30, 40, 41, 42, 43, 50, 51, 52, 60, 61, 62, 99, 4096,
];

/// Un argumento "interesante".
fn arg(r: &mut Rng, buf: u64) -> u64 {
    match r.below(10) {
        0 => 0,
        1 => r.below(8),                              // handles chicos, generación 0
        2 => r.below(64) | (r.below(4) << 20),        // handles con generación
        3 => buf + r.below(256),                      // mi pila (escribible)
        4 => 0x40_0000 + r.below(0x1000),             // mi código (solo lectura)
        5 => KERNEL_ADDR + r.below(1 << 20),          // el kernel
        6 => 0x0000_7fff_ffff_f000 + r.below(0x2000), // borde del espacio de usuario
        7 => u64::MAX - r.below(16),
        8 => r.below(1 << 16), // tamaños y desplazamientos
        _ => r.next(),
    }
}

fn main() -> i32 {
    let mut buf = [0u8; 1024];
    let base = buf.as_mut_ptr() as u64;
    println!("caos: semilla {}", SEED);
    let mut rng = Rng(SEED);
    // Resultados: éxitos, NoSys, BadHandle, WrongType, AccessDenied,
    // InvalidArgs, WouldBlock, PeerClosed, NoMemory, Unsupported, BadMessage,
    // TimedOut y otros.
    let mut seen = [0u32; 13];
    for _ in 0..ROUNDS {
        let n = CALLS[rng.below(CALLS.len() as u64) as usize];
        let mut a = [0u64; 4];
        for x in a.iter_mut() {
            *x = arg(&mut rng, base);
        }
        // Acotar lo que bloquearía para siempre o acapararía memoria.
        if n == Syscall::ChannelSend as u64
            || n == Syscall::ChannelRecv as u64
            || n == Syscall::CellStatus as u64
        {
            a[2] |= NONBLOCK;
        }
        if n == Syscall::MemCreate as u64 {
            a[0] &= 0x1FFF;
        }
        if n == Syscall::Wait as u64 {
            a[2] = 0; // plazo 0: solo mira, nunca duerme
        }
        if n == Syscall::ChannelCall as u64 {
            a[2] = 0; // plazo 0: ni el envío ni la respuesta esperan
        }
        if n == Syscall::DebugLog as u64 && a[0] < 0x0000_8000_0000_0000 {
            // No llenar la consola de basura: aquí solo punteros inválidos
            // (los válidos ya los prueba `intruso`).
            a[0] = KERNEL_ADDR;
        }
        let raw = raw_syscall(n, a[0], a[1], a[2], a[3]);
        // Un código negativo fuera del ABI sería un bug del kernel
        // (`decode_ret` lo convertiría en `InvalidArgs` y lo escondería).
        let i = if raw >= 0 {
            0
        } else {
            match u16::try_from(raw.unsigned_abs())
                .ok()
                .and_then(Error::from_code)
            {
                Some(Error::NoSys) => 1,
                Some(Error::BadHandle) => 2,
                Some(Error::WrongType) => 3,
                Some(Error::AccessDenied) => 4,
                Some(Error::InvalidArgs) => 5,
                Some(Error::WouldBlock) => 6,
                Some(Error::PeerClosed) => 7,
                Some(Error::NoMemory) => 8,
                Some(Error::Unsupported) => 9,
                Some(Error::BadMessage) => 10,
                Some(Error::TimedOut) => 11,
                None => 12,
            }
        };
        seen[i] += 1;
    }
    core::hint::black_box(&buf);
    println!(
        "caos: {} syscalls al azar; ok {}, NoSys {}, BadHandle {}, WrongType {}, AccessDenied {}, InvalidArgs {}, WouldBlock {}, PeerClosed {}, NoMemory {}, Unsupported {}, BadMessage {}, TimedOut {}, otros {}",
        ROUNDS, seen[0], seen[1], seen[2], seen[3], seen[4], seen[5], seen[6], seen[7], seen[8], seen[9], seen[10], seen[11], seen[12]
    );
    // Un resultado fuera del ABI sería un bug del kernel.
    if seen[12] > 0 {
        return 1;
    }
    0
}
