//! Descriptores de tablas de páginas ARM64 (VMSAv8-64, granulo de 4 KiB,
//! 48 bits, 4 niveles).
//!
//! El recorrido de tablas de [`crate::paging`] (y todas sus pruebas) trabaja
//! con entradas *neutras*: los bits de permiso de x86_64 (`PRESENT`,
//! `WRITABLE`, `USER`, `NO_EXECUTE`, `HUGE`...). En ARM64 los índices son los
//! mismos (9 bits por nivel, desplazamiento de 12), pero el formato de los
//! descriptores es otro. [`Arm64`] envuelve las tablas reales y traduce en
//! cada lectura y escritura con [`encode`] y [`decode`], así el recorrido no
//! cambia.
//!
//! Lo que hay que creer para que el aislamiento valga en ARM64 es que **lo
//! que el kernel ve** (las entradas decodificadas, que usa `usercopy` para
//! decidir si una cell puede tocar una dirección) **es lo que hace cumplir la
//! MMU**. [`hw_walk`] es un modelo de la MMU escrito a partir del manual de
//! ARM (ARM DDI 0487, D8: traducción de etapa 1 en EL1&0), independiente del
//! codec, y las pruebas de Kani de este módulo demuestran que coinciden.
//!
//! ## Codificación
//!
//! Un descriptor de tabla ignora los bits 2..=11 y 52..=58, y uno de página
//! o bloque ignora los bits 59..=62 (PBHA, inactivo con `TCR_EL1.HWU* = 0`).
//! Por eso la misma codificación sirve para los tres tipos sin saber el
//! nivel: los atributos de hoja y las restricciones jerárquicas (`APTable`,
//! `UXNTable`, `PXNTable`) van juntos y el hardware lee los que corresponden.
//!
//! | Neutro | ARM64 |
//! |---|---|
//! | `PRESENT` | bit 0 (válido) |
//! | sin `HUGE` | bit 1 = 1 (tabla, o página en el nivel 3) |
//! | `HUGE` | bit 1 = 0 (bloque) |
//! | `USER` | `AP[1]` = 1; sin `USER`: `APTable[0]` = 1 |
//! | sin `WRITABLE` | `AP[2]` = 1 y `APTable[1]` = 1 |
//! | `NO_EXECUTE` | `PXN`, `UXN`, `PXNTable`, `UXNTable` |
//! | sin `USER` | además `UXN` y `UXNTable` (EL0 no ejecuta el kernel) |
//! | `USER` | además `PXN` (EL1 no ejecuta código de cells: como SMEP) |
//! | sin `GLOBAL` | `nG` |
//! | `NO_CACHE` | `AttrIndx` = 1 (Device-nGnRnE en `MAIR_EL1`) |
//! | `WRITE_THROUGH` | bit 55 (de software; solo para decodificar igual) |
//!
//! Siempre se ponen `AF` (sin fallos de acceso) y `SH` = interno compartible.

use crate::paging::{flags, Tables};

/// Bits de un descriptor ARM64.
pub mod desc {
    /// Descriptor válido.
    pub const VALID: u64 = 1 << 0;
    /// Tabla (niveles 0..2) o página (nivel 3); en 0, bloque.
    pub const TABLE: u64 = 1 << 1;
    /// `AttrIndx[2:0]`: índice en `MAIR_EL1`.
    pub const ATTR_SHIFT: u32 = 2;
    /// Máscara de `AttrIndx`.
    pub const ATTR_MASK: u64 = 0b111 << ATTR_SHIFT;
    /// `AP[1]`: accesible desde EL0.
    pub const AP_EL0: u64 = 1 << 6;
    /// `AP[2]`: solo lectura.
    pub const AP_RO: u64 = 1 << 7;
    /// `SH[1:0]` = 0b11: interno compartible.
    pub const SH_INNER: u64 = 0b11 << 8;
    /// Access flag.
    pub const AF: u64 = 1 << 10;
    /// No global (la entrada del TLB depende del ASID).
    pub const NG: u64 = 1 << 11;
    /// Dirección de salida, bits 47..12.
    pub const ADDR: u64 = 0x0000_ffff_ffff_f000;
    /// Privileged execute-never (EL1).
    pub const PXN: u64 = 1 << 53;
    /// Unprivileged execute-never (EL0).
    pub const UXN: u64 = 1 << 54;
    /// Bit de software: la entrada neutra tenía `WRITE_THROUGH`.
    pub const SW_WT: u64 = 1 << 55;
    /// `PXNTable`: nada por debajo se ejecuta en EL1.
    pub const PXN_TABLE: u64 = 1 << 59;
    /// `UXNTable`: nada por debajo se ejecuta en EL0.
    pub const UXN_TABLE: u64 = 1 << 60;
    /// `APTable[0]`: nada por debajo es accesible desde EL0.
    pub const AP_TABLE_NO_EL0: u64 = 1 << 61;
    /// `APTable[1]`: nada por debajo se puede escribir.
    pub const AP_TABLE_RO: u64 = 1 << 62;
}

/// Índice de `MAIR_EL1` para memoria normal (write-back).
pub const ATTR_NORMAL: u64 = 0;
/// Índice de `MAIR_EL1` para registros de dispositivos (Device-nGnRnE).
pub const ATTR_DEVICE: u64 = 1;
/// Valor de `MAIR_EL1` que corresponde a [`ATTR_NORMAL`] (byte 0 = 0xFF,
/// write-back) y [`ATTR_DEVICE`] (byte 1 = 0x00, Device-nGnRnE).
pub const MAIR: u64 = 0xFF;

/// Bits neutros que el codec conserva (el resto se pierde).
pub const NEUTRAL_BITS: u64 = flags::PRESENT
    | flags::WRITABLE
    | flags::USER
    | flags::WRITE_THROUGH
    | flags::NO_CACHE
    | flags::HUGE
    | flags::GLOBAL
    | flags::NO_EXECUTE
    | desc::ADDR;

/// Codifica una entrada neutra como descriptor ARM64. Una entrada no
/// presente se codifica como 0 (inválida).
pub fn encode(n: u64) -> u64 {
    if n & flags::PRESENT == 0 {
        return 0;
    }
    let user = n & flags::USER != 0;
    let writable = n & flags::WRITABLE != 0;
    let nx = n & flags::NO_EXECUTE != 0;
    let mut d = desc::VALID | (n & desc::ADDR) | desc::AF | desc::SH_INNER;
    if n & flags::HUGE == 0 {
        d |= desc::TABLE;
    }
    if n & flags::NO_CACHE != 0 {
        d |= ATTR_DEVICE << desc::ATTR_SHIFT;
    }
    if n & flags::WRITE_THROUGH != 0 {
        d |= desc::SW_WT;
    }
    if n & flags::GLOBAL == 0 {
        d |= desc::NG;
    }
    if user {
        d |= desc::AP_EL0;
    } else {
        d |= desc::AP_TABLE_NO_EL0;
    }
    if !writable {
        d |= desc::AP_RO | desc::AP_TABLE_RO;
    }
    if nx || !user {
        d |= desc::UXN | desc::UXN_TABLE;
    }
    if nx || user {
        d |= desc::PXN;
    }
    if nx {
        d |= desc::PXN_TABLE;
    }
    d
}

/// Decodifica un descriptor ARM64 como entrada neutra. Un descriptor
/// inválido se decodifica como 0.
pub fn decode(d: u64) -> u64 {
    if d & desc::VALID == 0 {
        return 0;
    }
    let mut n = flags::PRESENT | (d & desc::ADDR);
    let user = d & desc::AP_EL0 != 0;
    if d & desc::TABLE == 0 {
        n |= flags::HUGE;
    }
    if user {
        n |= flags::USER;
    }
    if d & desc::AP_RO == 0 {
        n |= flags::WRITABLE;
    }
    if (d & desc::ATTR_MASK) >> desc::ATTR_SHIFT == ATTR_DEVICE {
        n |= flags::NO_CACHE;
    }
    if d & desc::SW_WT != 0 {
        n |= flags::WRITE_THROUGH;
    }
    if d & desc::NG == 0 {
        n |= flags::GLOBAL;
    }
    // La protección de ejecución que pidió el kernel es la del nivel que la
    // usa: UXN para una página de cell, PXN para una del kernel.
    let xn = if user { desc::UXN } else { desc::PXN };
    if d & xn != 0 {
        n |= flags::NO_EXECUTE;
    }
    n
}

/// Tablas ARM64 vistas como tablas neutras: lee y escribe a través del
/// codec. `T` accede a los descriptores reales.
pub struct Arm64<T>(pub T);

impl<T: Tables> Tables for Arm64<T> {
    fn read(&self, table: u64, i: usize) -> u64 {
        decode(self.0.read(table, i))
    }
    fn write(&mut self, table: u64, i: usize, v: u64) {
        self.0.write(table, i, encode(v));
    }
}

/// Permisos efectivos de una dirección según la MMU.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HwPerms {
    /// EL0 (cells) puede leer.
    pub el0_read: bool,
    /// EL0 puede escribir.
    pub el0_write: bool,
    /// EL0 puede ejecutar.
    pub el0_exec: bool,
    /// EL1 (kernel) puede escribir (leer siempre puede, salvo PAN).
    pub el1_write: bool,
    /// EL1 puede ejecutar.
    pub el1_exec: bool,
    /// Con `PSTATE.PAN` = 1, un acceso a datos de EL1 falla.
    pub pan_blocks_el1: bool,
}

/// Modelo de la MMU (etapa 1, EL1&0, granulo de 4 KiB, 48 bits, nivel
/// inicial 0), sobre los descriptores reales. Devuelve la física y los
/// permisos, o `None` si la traducción falla.
///
/// Es deliberadamente conservador donde el manual da más protección (no
/// modela `SCTLR_EL1.WXN` ni la regla de que lo escribible en EL0 es PXN):
/// así las pruebas no dependen de esas protecciones extra.
pub fn hw_walk<T: Tables + ?Sized>(t: &T, root: u64, va: u64) -> Option<(u64, HwPerms)> {
    let mut table = root;
    let (mut no_el0, mut ro, mut uxn_t, mut pxn_t) = (false, false, false, false);
    // Nivel ARM 0..=3 (el nivel neutro es 4 - nivel).
    let mut level = 0u32;
    loop {
        let shift = 39 - 9 * level;
        let d = t.read(table, ((va >> shift) & 0x1ff) as usize);
        if d & desc::VALID == 0 {
            return None; // fallo de traducción
        }
        let is_table_bit = d & desc::TABLE != 0;
        if level < 3 && is_table_bit {
            no_el0 |= d & desc::AP_TABLE_NO_EL0 != 0;
            ro |= d & desc::AP_TABLE_RO != 0;
            uxn_t |= d & desc::UXN_TABLE != 0;
            pxn_t |= d & desc::PXN_TABLE != 0;
            table = d & desc::ADDR;
            level += 1;
            continue;
        }
        // Bloque en el nivel 0 (no existe con 4 KiB) o descriptor reservado
        // en el nivel 3: fallo de traducción.
        if level == 0 || (level == 3 && !is_table_bit) {
            return None;
        }
        if d & desc::AF == 0 {
            return None; // fallo de access flag
        }
        let size: u64 = 1 << shift;
        let pa = (d & desc::ADDR & !(size - 1)) | (va & (size - 1));
        let el0 = d & desc::AP_EL0 != 0 && !no_el0;
        let w = d & desc::AP_RO == 0 && !ro;
        let perms = HwPerms {
            el0_read: el0,
            el0_write: el0 && w,
            el0_exec: d & desc::UXN == 0 && !uxn_t,
            el1_write: w,
            el1_exec: d & desc::PXN == 0 && !pxn_t,
            pan_blocks_el1: el0,
        };
        return Some((pa, perms));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paging::{map, translate_with_flags, PageSize, PAGE_SIZE};
    use alloc::vec;
    use alloc::vec::Vec;

    struct Raw(Vec<[u64; 512]>);

    impl Tables for Raw {
        fn read(&self, t: u64, i: usize) -> u64 {
            self.0[(t / PAGE_SIZE - 1) as usize][i]
        }
        fn write(&mut self, t: u64, i: usize, v: u64) {
            self.0[(t / PAGE_SIZE - 1) as usize][i] = v;
        }
    }

    fn arm_map(m: &mut Arm64<Raw>, v: u64, p: u64, s: PageSize, f: u64) {
        let mut n = m.0 .0.len() as u64;
        m.0 .0.extend([[0; 512]; 3]);
        map(m, PAGE_SIZE, v, p, s, f, &mut || {
            n += 1;
            Some(n * PAGE_SIZE)
        })
        .unwrap();
        m.0 .0.truncate(n as usize);
    }

    #[test]
    fn roundtrip_examples() {
        for n in [
            0,
            flags::PRESENT,
            flags::PRESENT | flags::USER | flags::WRITABLE | flags::NO_EXECUTE | 0x1234_5000,
            flags::PRESENT | flags::HUGE | flags::GLOBAL | 0x20_0000,
            flags::PRESENT | flags::NO_CACHE | flags::WRITE_THROUGH | flags::WRITABLE,
        ] {
            assert_eq!(decode(encode(n)), n, "{n:#x}");
        }
    }

    #[test]
    fn user_and_kernel_pages() {
        let mut m = Arm64(Raw(vec![[0; 512]]));
        let user = flags::USER | flags::WRITABLE | flags::NO_EXECUTE;
        arm_map(&mut m, 0x40_0000, 0x8000, PageSize::Size4K, user);
        arm_map(&mut m, 0x40_1000, 0x9000, PageSize::Size4K, flags::USER);
        arm_map(
            &mut m,
            0x60_0000,
            0x20_0000,
            PageSize::Size2M,
            flags::WRITABLE,
        );

        let (pa, p) = hw_walk(&m.0, PAGE_SIZE, 0x40_0010).unwrap();
        assert_eq!(pa, 0x8010);
        assert!(p.el0_read && p.el0_write && !p.el0_exec && !p.el1_exec && p.pan_blocks_el1);

        let (_, p) = hw_walk(&m.0, PAGE_SIZE, 0x40_1000).unwrap();
        assert!(p.el0_read && !p.el0_write && p.el0_exec && !p.el1_exec);

        let (pa, p) = hw_walk(&m.0, PAGE_SIZE, 0x61_2345).unwrap();
        assert_eq!(pa, 0x21_2345);
        assert!(!p.el0_read && !p.el0_exec && p.el1_write && p.el1_exec && !p.pan_blocks_el1);

        // La vista neutra coincide.
        let (pa, f) = translate_with_flags(&m, PAGE_SIZE, 0x40_0010).unwrap();
        assert_eq!(pa, 0x8010);
        assert_eq!(
            f & (flags::USER | flags::WRITABLE),
            flags::USER | flags::WRITABLE
        );
        assert_eq!(hw_walk(&m.0, PAGE_SIZE, 0x50_0000), None);
    }
}

/// Pruebas de Kani (ver `verification/kani.md`).
#[cfg(kani)]
mod proofs {
    use super::*;
    use crate::paging::{
        index, map, translate_with_flags, PageSize, ADDR_MASK, KERNEL_HALF, PAGE_SIZE,
    };

    fn any_neutral() -> u64 {
        let n: u64 = kani::any();
        kani::assume(n & !NEUTRAL_BITS == 0);
        n
    }

    /// Toda entrada neutra presente (con dirección de hasta 48 bits) se
    /// codifica y decodifica igual; una no presente queda en 0.
    #[kani::proof]
    fn codec_roundtrip() {
        let n = any_neutral();
        if n & flags::PRESENT != 0 {
            assert_eq!(decode(encode(n)), n);
        } else {
            assert_eq!(encode(n), 0);
        }
    }

    /// Una cadena de cuatro entradas neutras cualesquiera (una por nivel),
    /// codificadas: la raíz está en 4096, la tabla del nivel siguiente en
    /// 8192, etc., y cualquier índice devuelve la entrada de esa tabla.
    /// Otras direcciones están vacías (no presentes).
    struct Chain([u64; 4]);

    impl Tables for Chain {
        fn read(&self, table: u64, i: usize) -> u64 {
            assert!(i < 512);
            match table {
                0x1000 => encode(self.0[0]),
                0x2000 => encode(self.0[1]),
                0x3000 => encode(self.0[2]),
                0x4000 => encode(self.0[3]),
                _ => 0,
            }
        }
        fn write(&mut self, _: u64, _: usize, _: u64) {
            unreachable!()
        }
    }

    /// Con tablas cualesquiera escritas por el codec, lo que ve el kernel
    /// (`translate_with_flags` sobre las entradas decodificadas) y lo que
    /// hace cumplir la MMU coinciden: la misma física, y EL0 puede leer,
    /// escribir o ejecutar exactamente cuando la vista neutra lo permite.
    /// Además EL1 nunca ejecuta una página de cell (SMEP), nunca escribe
    /// lo que la vista marca de solo lectura, y con PAN no toca una página
    /// de cell (SMAP).
    ///
    /// Supuesto (lo cumple `paging::map`, que solo crea hojas grandes en el
    /// nivel 2): no hay `HUGE` en el nivel 4 ni en el 1.
    #[kani::proof]
    #[kani::unwind(6)]
    fn hw_agrees_with_view() {
        let chain = Chain([any_neutral(), any_neutral(), any_neutral(), any_neutral()]);
        // chain.0[0] es la entrada del nivel 4 (raíz), chain.0[3] la del 1.
        kani::assume(chain.0[0] & flags::HUGE == 0 && chain.0[3] & flags::HUGE == 0);
        // Una entrada que apunta a una de las cuatro tablas apunta a la del
        // nivel siguiente (las tablas forman un árbol, sin ciclos). Si
        // apunta a otro lado, la lectura siguiente da "no presente".
        for (j, &e) in chain.0[..3].iter().enumerate() {
            let a = e & desc::ADDR;
            if (0x1000..=0x4000).contains(&a) {
                kani::assume(a == (j as u64 + 2) * 0x1000);
            }
        }
        let va: u64 = kani::any();
        kani::assume(va < 1 << 48);
        let view = translate_with_flags(&Arm64(&chain), 0x1000, va);
        let hw = hw_walk(&chain, 0x1000, va);
        match (view, hw) {
            (None, None) => {}
            (Some((vpa, f)), Some((hpa, p))) => {
                assert_eq!(vpa, hpa);
                let user = f & flags::USER != 0;
                let w = f & flags::WRITABLE != 0;
                let nx = f & flags::NO_EXECUTE != 0;
                assert_eq!(p.el0_read, user);
                assert_eq!(p.el0_write, user && w);
                assert_eq!(p.el0_exec, user && !nx);
                assert_eq!(p.el1_write, w);
                assert!(!(p.el1_exec && (nx || user)));
                assert_eq!(p.pan_blocks_el1, user);
            }
            _ => panic!("la vista y la MMU no coinciden en si la dirección está mapeada"),
        }
    }

    /// `Chain` a través de una referencia (para envolverla sin moverla).
    impl Tables for &Chain {
        fn read(&self, table: u64, i: usize) -> u64 {
            (**self).read(table, i)
        }
        fn write(&mut self, _: u64, _: usize, _: u64) {
            unreachable!()
        }
    }

    /// Tablas reales guardadas como lista de escrituras de descriptores
    /// (como en `paging::proofs`). La tabla 1 (en 4096) es la raíz.
    const T: usize = 6;
    const W: usize = 8;

    struct Log {
        log: [(u64, usize, u64); W],
        len: usize,
    }

    impl Tables for Log {
        fn read(&self, table: u64, i: usize) -> u64 {
            assert!(i < 512);
            let mut v = 0;
            for j in 0..W {
                if j < self.len && self.log[j].0 == table && self.log[j].1 == i {
                    v = self.log[j].2;
                }
            }
            v
        }
        fn write(&mut self, table: u64, i: usize, v: u64) {
            assert!(i < 512);
            let k = (table / PAGE_SIZE) as usize;
            assert!(table.is_multiple_of(PAGE_SIZE) && k >= 1 && k <= T);
            assert!(self.len < W);
            self.log[self.len] = (table, i, v);
            self.len += 1;
        }
    }

    /// De punta a punta: mapear una página de cell con el recorrido real
    /// sobre el codec deja en la MMU exactamente los permisos pedidos (y
    /// la física pedida); una página sin `USER` no es accesible desde EL0
    /// ni ejecutable desde EL0.
    #[kani::proof]
    #[kani::unwind(10)]
    #[kani::solver(cadical)]
    fn map_then_hw() {
        let mut m = Arm64(Log {
            log: [(0, 0, 0); W],
            len: 0,
        });
        let virt: u64 = kani::any();
        let phys: u64 = kani::any();
        kani::assume(phys & !ADDR_MASK == 0 && phys < 1 << 48);
        kani::assume(index(virt, 4) < KERNEL_HALF);
        let fl: u64 = kani::any();
        kani::assume(fl & !(flags::USER | flags::WRITABLE | flags::NO_EXECUTE) == 0);
        let mut next = 1;
        let r = map(
            &mut m,
            PAGE_SIZE,
            virt,
            phys,
            PageSize::Size4K,
            fl,
            &mut || {
                if next < T {
                    next += 1;
                    Some(next as u64 * PAGE_SIZE)
                } else {
                    None
                }
            },
        );
        if r.is_ok() {
            let (pa, p) = hw_walk(&m.0, PAGE_SIZE, virt).unwrap();
            assert_eq!(pa, phys);
            let user = fl & flags::USER != 0;
            let w = fl & flags::WRITABLE != 0;
            let nx = fl & flags::NO_EXECUTE != 0;
            assert_eq!(p.el0_read, user);
            assert_eq!(p.el0_write, user && w);
            assert_eq!(p.el0_exec, user && !nx);
            assert_eq!(p.el1_write, w);
            assert_eq!(p.el1_exec, !user && !nx);
        }
    }
}
