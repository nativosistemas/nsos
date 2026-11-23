//! Recorrido de tablas de páginas x86_64 de 4 niveles, sin acceso directo a
//! memoria.
//!
//! El kernel (`kernel/src/arch/x86_64/paging.rs`) implementa [`Tables`]
//! leyendo y escribiendo las tablas a través del mapeo directo de memoria
//! física. Aquí queda la lógica (índices, permisos de los niveles
//! intermedios, páginas grandes), para probarla en el host y con Kani (ver
//! `verification/kani.md`).

use nsos_abi::Rights;

/// Tamaño de página base.
pub const PAGE_SIZE: u64 = 4096;
/// Tamaño de página grande.
pub const HUGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;
/// Bits de dirección física de una entrada.
pub const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
/// Primera entrada del PML4 que pertenece a la mitad alta (el kernel).
pub const KERNEL_HALF: usize = 256;

/// Bits de una entrada de tabla de páginas.
pub mod flags {
    /// La entrada es válida.
    pub const PRESENT: u64 = 1 << 0;
    /// Se puede escribir.
    pub const WRITABLE: u64 = 1 << 1;
    /// Accesible desde anillo 3.
    pub const USER: u64 = 1 << 2;
    /// Escritura directa (sin write-back).
    pub const WRITE_THROUGH: u64 = 1 << 3;
    /// Sin caché.
    pub const NO_CACHE: u64 = 1 << 4;
    /// Página grande (2 MiB en nivel 2, 1 GiB en nivel 3).
    pub const HUGE: u64 = 1 << 7;
    /// Global (no se borra del TLB al cambiar de espacio).
    pub const GLOBAL: u64 = 1 << 8;
    /// No ejecutable.
    pub const NO_EXECUTE: u64 = 1 << 63;
}

/// Errores al mapear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// No hay marcos para tablas intermedias.
    OutOfFrames,
    /// La dirección ya estaba mapeada.
    AlreadyMapped,
    /// Dirección no alineada al tamaño de página pedido.
    Misaligned,
    /// Choca con una página grande existente.
    HugePageConflict,
}

/// Tamaño de una página a mapear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSize {
    /// 4 KiB.
    Size4K,
    /// 2 MiB.
    Size2M,
}

impl PageSize {
    /// Tamaño en bytes.
    pub const fn bytes(self) -> u64 {
        match self {
            PageSize::Size4K => PAGE_SIZE,
            PageSize::Size2M => HUGE_PAGE_SIZE,
        }
    }
}

/// Acceso a las tablas: `table` es la dirección física de una tabla (de 512
/// entradas) y `i < 512`.
pub trait Tables {
    /// Lee la entrada `i` de la tabla `table`.
    fn read(&self, table: u64, i: usize) -> u64;
    /// Escribe la entrada `i` de la tabla `table`.
    fn write(&mut self, table: u64, i: usize, v: u64);
}

/// Índice de `virt` en la tabla de nivel `level` (1 = PT, 4 = PML4).
pub fn index(virt: u64, level: u32) -> usize {
    ((virt >> (12 + 9 * (level - 1))) & 0x1ff) as usize
}

/// Mapea una página `virt → phys` con `fl` (PRESENT se agrega solo).
/// `alloc` devuelve marcos físicos para tablas nuevas, ya puestos a cero.
pub fn map<M: Tables + ?Sized>(
    m: &mut M,
    root: u64,
    virt: u64,
    phys: u64,
    size: PageSize,
    fl: u64,
    alloc: &mut dyn FnMut() -> Option<u64>,
) -> Result<(), MapError> {
    if !virt.is_multiple_of(size.bytes()) || !phys.is_multiple_of(size.bytes()) {
        return Err(MapError::Misaligned);
    }
    let leaf_level = match size {
        PageSize::Size4K => 1,
        PageSize::Size2M => 2,
    };
    let inter = flags::PRESENT | flags::WRITABLE | (fl & flags::USER);
    let mut table = root;
    let mut level = 4;
    while level > leaf_level {
        let i = index(virt, level);
        let e = m.read(table, i);
        let next = if e & flags::PRESENT == 0 {
            let f = alloc().ok_or(MapError::OutOfFrames)?;
            m.write(table, i, f | inter);
            f
        } else if e & flags::HUGE != 0 {
            return Err(MapError::HugePageConflict);
        } else {
            if e | inter != e {
                m.write(table, i, e | inter);
            }
            e & ADDR_MASK
        };
        table = next;
        level -= 1;
    }
    let i = index(virt, leaf_level);
    if m.read(table, i) & flags::PRESENT != 0 {
        return Err(MapError::AlreadyMapped);
    }
    let huge = if size == PageSize::Size2M {
        flags::HUGE
    } else {
        0
    };
    m.write(table, i, phys | fl | flags::PRESENT | huge);
    Ok(())
}

/// Quita el mapeo de una página de 4 KiB. Devuelve la física que había. No
/// invalida el TLB: eso le toca al llamador.
pub fn unmap<M: Tables + ?Sized>(m: &mut M, root: u64, virt: u64) -> Option<u64> {
    let mut table = root;
    let mut level = 4;
    while level > 1 {
        let e = m.read(table, index(virt, level));
        if e & flags::PRESENT == 0 || e & flags::HUGE != 0 {
            return None;
        }
        table = e & ADDR_MASK;
        level -= 1;
    }
    let i = index(virt, 1);
    let e = m.read(table, i);
    if e & flags::PRESENT == 0 {
        return None;
    }
    m.write(table, i, 0);
    Some(e & ADDR_MASK)
}

/// Traduce y devuelve los permisos *efectivos* (USER y WRITABLE solo si
/// están en todos los niveles; NX si está en alguno), como los ve la CPU.
pub fn translate_with_flags<M: Tables + ?Sized>(m: &M, root: u64, virt: u64) -> Option<(u64, u64)> {
    let mut table = root;
    let mut eff = flags::USER | flags::WRITABLE;
    let mut nx = 0;
    let mut level = 4;
    loop {
        let e = m.read(table, index(virt, level));
        if e & flags::PRESENT == 0 {
            return None;
        }
        eff &= e;
        nx |= e & flags::NO_EXECUTE;
        if level == 1 || e & flags::HUGE != 0 {
            let page: u64 = match level {
                1 => PAGE_SIZE,
                2 => HUGE_PAGE_SIZE,
                _ => 1 << 30,
            };
            let base = e & ADDR_MASK & !(page - 1);
            return Some((base + (virt & (page - 1)), eff | nx | flags::PRESENT));
        }
        table = e & ADDR_MASK;
        level -= 1;
    }
}

/// Traduce una dirección virtual a física.
pub fn translate<M: Tables + ?Sized>(m: &M, root: u64, virt: u64) -> Option<u64> {
    translate_with_flags(m, root, virt).map(|(p, _)| p)
}

/// ¿Puede una cell mapear `[vaddr, vaddr + size)`? Alineado a página, dentro
/// de `[min, max)` y sin desbordar. `map_memory` del kernel lo exige antes de
/// tocar las tablas; con `max` en la mitad baja (`elf::USER_MAX`), toda
/// página del rango cae en una entrada del PML4 menor que [`KERNEL_HALF`].
pub fn user_range_ok(vaddr: u64, size: u64, min: u64, max: u64) -> bool {
    vaddr.is_multiple_of(PAGE_SIZE)
        && vaddr >= min
        && match vaddr.checked_add(size) {
            Some(end) => end <= max,
            None => false,
        }
}

/// Lo que pide `mem_map` una vez validado: los derechos que tiene que tener
/// la capacidad y si el mapeo es escribible. No hay campo de ejecución: un
/// objeto `Memory` nunca se mapea ejecutable (cells selladas; modelo:
/// `Mem.step_sealed`, `Mem.exec_obj_breaks`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapRequest {
    /// Derechos que tiene que tener la capacidad (`READ`, y `WRITE` si
    /// `write`).
    pub need: Rights,
    /// El mapeo es escribible.
    pub write: bool,
}

/// Valida los derechos que pide `mem_map(h, vaddr, rights)`: siempre `READ`,
/// opcionalmente `WRITE` y nada más. `EXEC` (o cualquier otro bit) se
/// rechaza: el código de una cell es solo el de su imagen ELF.
pub fn map_request(want: u64) -> Option<MapRequest> {
    let allowed = Rights::READ.bits() as u64 | Rights::WRITE.bits() as u64;
    if want & !allowed != 0 || want & Rights::READ.bits() as u64 == 0 {
        return None;
    }
    let need = Rights(want as u32);
    Some(MapRequest {
        need,
        write: need.contains(Rights::WRITE),
    })
}

/// `mem_unmap(vaddr, len)`: cuál de los mapeos de objetos `Memory` de la
/// cell (`(dirección, tamaño)` de cada uno) se quita. Tiene que coincidir
/// exactamente con uno: misma dirección y mismo tamaño (un objeto se mapea y
/// se desmapea entero). Así nunca se quita una página de código, de la pila
/// ni de otro objeto, y nunca queda medio objeto mapeado.
pub fn unmap_target(maps: &[(u64, u64)], vaddr: u64, len: u64) -> Option<usize> {
    if len == 0 || !vaddr.is_multiple_of(PAGE_SIZE) {
        return None;
    }
    maps.iter().position(|&(v, l)| v == vaddr && l == len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn map_request_rejects_exec() {
        let (r, w, x) = (
            Rights::READ.bits() as u64,
            Rights::WRITE.bits() as u64,
            Rights::EXEC.bits() as u64,
        );
        assert_eq!(
            map_request(r),
            Some(MapRequest {
                need: Rights::READ,
                write: false
            })
        );
        assert_eq!(map_request(r | w).map(|m| m.write), Some(true));
        // Ni RX ni RWX: la memoria nunca es ejecutable.
        assert_eq!(map_request(r | x), None);
        assert_eq!(map_request(r | w | x), None);
        // Sin READ, o con bits ajenos, tampoco.
        assert_eq!(map_request(w), None);
        assert_eq!(map_request(r | 1 << 40), None);
    }

    #[test]
    fn unmap_target_matches_exactly() {
        let maps = [(0x5000_0000, 0x2000), (0x6000_0000, 0x1000)];
        assert_eq!(unmap_target(&maps, 0x6000_0000, 0x1000), Some(1));
        assert_eq!(unmap_target(&maps, 0x5000_0000, 0x1000), None); // medio objeto
        assert_eq!(unmap_target(&maps, 0x5000_1000, 0x1000), None);
        assert_eq!(unmap_target(&maps, 0x5000_0000, 0), None);
        assert_eq!(unmap_target(&maps, 0x7000_0000, 0x1000), None);
    }
    use alloc::vec::Vec;

    /// Tablas en un vector: la tabla `k` está en la física `(k + 1) * 4096`.
    struct Mem(Vec<[u64; 512]>);

    impl Tables for Mem {
        fn read(&self, t: u64, i: usize) -> u64 {
            self.0[(t / PAGE_SIZE - 1) as usize][i]
        }
        fn write(&mut self, t: u64, i: usize, v: u64) {
            self.0[(t / PAGE_SIZE - 1) as usize][i] = v;
        }
    }

    impl Mem {
        fn new() -> Self {
            Mem(vec![[0; 512]])
        }
        /// Mapea con tablas nuevas tomadas del final del vector.
        fn map(&mut self, v: u64, p: u64, s: PageSize, f: u64) -> Result<(), MapError> {
            let mut n = self.0.len() as u64;
            self.0.extend([[0; 512]; 3]);
            let r = map(self, PAGE_SIZE, v, p, s, f, &mut || {
                n += 1;
                Some(n * PAGE_SIZE)
            });
            self.0.truncate(n as usize);
            r
        }
    }

    #[test]
    fn map_translate_unmap() {
        let mut m = Mem::new();
        let f = flags::USER | flags::WRITABLE;
        m.map(0x40_0000, 0x1234_5000, PageSize::Size4K, f).unwrap();
        assert_eq!(translate(&m, PAGE_SIZE, 0x40_0123), Some(0x1234_5123));
        let (_, eff) = translate_with_flags(&m, PAGE_SIZE, 0x40_0000).unwrap();
        assert_eq!(eff & (flags::USER | flags::WRITABLE), f);
        assert_eq!(
            m.map(0x40_0000, 0x9000, PageSize::Size4K, f),
            Err(MapError::AlreadyMapped)
        );
        assert_eq!(unmap(&mut m, PAGE_SIZE, 0x40_0000), Some(0x1234_5000));
        assert_eq!(translate(&m, PAGE_SIZE, 0x40_0000), None);
    }

    #[test]
    fn huge_pages() {
        let mut m = Mem::new();
        m.map(0x20_0000, 0x40_0000, PageSize::Size2M, 0).unwrap();
        assert_eq!(translate(&m, PAGE_SIZE, 0x20_1234), Some(0x40_1234));
        assert_eq!(
            m.map(0x20_1000, 0x1000, PageSize::Size4K, 0),
            Err(MapError::HugePageConflict)
        );
        assert_eq!(unmap(&mut m, PAGE_SIZE, 0x20_0000), None);
    }

    #[test]
    fn user_range() {
        assert!(user_range_ok(0x1000, 0x2000, 0x1000, 0x4000));
        assert!(!user_range_ok(0x1000, 0x4000, 0x1000, 0x4000));
        assert!(!user_range_ok(0x1001, 1, 0, 0x4000));
        assert!(!user_range_ok(!0xfff, 0x2000, 0, u64::MAX));
    }
}

/// Pruebas de Kani (ver `verification/kani.md`).
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Memoria arbitraria: cada lectura devuelve cualquier valor.
    struct AnyMem;

    impl Tables for AnyMem {
        fn read(&self, _: u64, i: usize) -> u64 {
            assert!(i < 512);
            kani::any()
        }
        fn write(&mut self, _: u64, i: usize, _: u64) {
            assert!(i < 512);
        }
    }

    /// Con tablas de contenido cualquiera (incluso corruptas), recorrerlas
    /// nunca entra en pánico ni desborda, y los índices están en rango.
    #[kani::proof]
    #[kani::unwind(6)]
    fn walk_any_tables() {
        let root: u64 = kani::any();
        let virt: u64 = kani::any();
        let _ = translate_with_flags(&AnyMem, root, virt);
        let _ = unmap(&mut AnyMem, root, virt);
        let mut alloc = || -> Option<u64> { kani::any() };
        let size = if kani::any() {
            PageSize::Size4K
        } else {
            PageSize::Size2M
        };
        let _ = map(
            &mut AnyMem,
            root,
            virt,
            kani::any(),
            size,
            kani::any(),
            &mut alloc,
        );
    }

    /// Tablas de verdad, guardadas como lista de escrituras (las entradas
    /// nunca escritas valen 0): la tabla `k` está en `(k + 1) * 4096`. La
    /// tabla 0 es el PML4 y la 1 es una tabla del kernel colgada de la
    /// entrada 256 (sin USER), como en `AddressSpace::new_user`.
    const T: usize = 8;
    const W: usize = 10;

    struct Mem {
        log: [(u64, usize, u64); W],
        len: usize,
        /// Se escribió en la mitad alta del PML4 o en una tabla del kernel.
        touched_kernel: bool,
        next: usize,
    }

    impl Tables for Mem {
        fn read(&self, table: u64, i: usize) -> u64 {
            assert!(i < 512);
            let k = (table / PAGE_SIZE) as usize;
            assert!(table.is_multiple_of(PAGE_SIZE) && k >= 1 && k <= T);
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
            if (k == 1 && i >= KERNEL_HALF) || k == 2 {
                self.touched_kernel = true;
            }
            assert!(self.len < W);
            self.log[self.len] = (table, i, v);
            self.len += 1;
        }
    }

    impl Mem {
        fn new() -> Self {
            let mut log = [(0, 0, 0); W];
            log[0] = (
                PAGE_SIZE,
                KERNEL_HALF,
                2 * PAGE_SIZE | flags::PRESENT | flags::WRITABLE,
            );
            Mem {
                log,
                len: 1,
                touched_kernel: false,
                next: 2,
            }
        }
        fn map4k(&mut self, virt: u64, phys: u64, fl: u64) -> Result<(), MapError> {
            // Las tablas nuevas salen de `next` (nunca escritas: a cero).
            let mut next = self.next;
            let r = map(
                self,
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
            self.next = next;
            r
        }
    }

    fn any_flags() -> u64 {
        let fl: u64 = kani::any();
        // Los llamadores solo pasan bits de permisos, nunca de dirección.
        kani::assume(fl & ADDR_MASK == 0 && fl & flags::HUGE == 0);
        fl
    }

    /// Mapear una página: después se traduce a esa física con exactamente
    /// los permisos pedidos, y si la dirección es de la mitad baja no se
    /// toca nada del kernel.
    #[kani::proof]
    #[kani::unwind(12)]
    #[kani::solver(cadical)]
    fn map_then_translate() {
        let mut m = Mem::new();
        let virt: u64 = kani::any();
        let phys: u64 = kani::any();
        kani::assume(phys & !ADDR_MASK == 0);
        let fl = any_flags();
        let user_half = index(virt, 4) < KERNEL_HALF;
        kani::assume(user_half);
        if m.map4k(virt, phys, fl).is_ok() {
            let (p, eff) = translate_with_flags(&m, PAGE_SIZE, virt).unwrap();
            assert_eq!(p, phys);
            assert_eq!(eff & flags::USER, fl & flags::USER);
            assert_eq!(eff & flags::WRITABLE, fl & flags::WRITABLE);
            assert_eq!(eff & flags::NO_EXECUTE, fl & flags::NO_EXECUTE);
        }
        assert!(!m.touched_kernel);
    }

    /// Dos mapeos y un `unmap` cualquiera: quita solo la página pedida.
    #[kani::proof]
    #[kani::unwind(12)]
    #[kani::solver(cadical)]
    fn unmap_only_that_page() {
        let mut m = Mem::new();
        let (a, b): (u64, u64) = (kani::any(), kani::any());
        // Direcciones canónicas de la mitad baja (los bits 47.. en cero; la
        // CPU no acepta otras, y `user_range_ok` tampoco).
        const LOW: u64 = 1 << 47;
        kani::assume(a < LOW && b < LOW);
        kani::assume(a / PAGE_SIZE != b / PAGE_SIZE);
        let (pa, pb) = (0x1000_0000, 0x2000_0000);
        if m.map4k(a, pa, flags::USER).is_err() || m.map4k(b, pb, flags::USER).is_err() {
            return;
        }
        let v: u64 = kani::any();
        kani::assume(v < LOW);
        let r = unmap(&mut m, PAGE_SIZE, v);
        if v / PAGE_SIZE == a / PAGE_SIZE {
            assert_eq!(r, Some(pa));
            assert_eq!(translate(&m, PAGE_SIZE, a), None);
            assert_eq!(translate(&m, PAGE_SIZE, b), Some(pb));
        } else if v / PAGE_SIZE != b / PAGE_SIZE {
            assert_eq!(r, None);
            assert_eq!(translate(&m, PAGE_SIZE, a), Some(pa));
            assert_eq!(translate(&m, PAGE_SIZE, b), Some(pb));
        }
        assert!(!m.touched_kernel);
    }

    /// `mem_unmap` solo quita un mapeo que coincide exactamente (dirección y
    /// tamaño), alineado y no vacío; si no hay ninguno, no quita nada.
    #[kani::proof]
    #[kani::unwind(4)]
    fn unmap_target_exact() {
        let maps: [(u64, u64); 3] = kani::any();
        let (vaddr, len): (u64, u64) = (kani::any(), kani::any());
        match unmap_target(&maps, vaddr, len) {
            Some(i) => {
                // Exactamente ese mapeo, y el primero que coincide.
                assert!(i < maps.len());
                assert!(maps[i] == (vaddr, len));
                assert!(len > 0 && vaddr % PAGE_SIZE == 0);
                assert!(maps[..i].iter().all(|&m| m != (vaddr, len)));
            }
            None => assert!(
                len == 0 || vaddr % PAGE_SIZE != 0 || maps.iter().all(|&m| m != (vaddr, len))
            ),
        }
    }

    /// Un rango que `user_range_ok` acepta no desborda y queda entero en la
    /// mitad baja: ninguna página cae en una entrada del PML4 del kernel.
    #[kani::proof]
    fn map_request_never_exec() {
        let want: u64 = kani::any();
        match map_request(want) {
            Some(r) => {
                // Nunca ejecutable, siempre legible, y exactamente lo pedido.
                assert!(want & Rights::EXEC.bits() as u64 == 0);
                assert!(!r.need.contains(Rights::EXEC));
                assert!(r.need.contains(Rights::READ));
                assert_eq!(r.need.bits() as u64, want);
                assert_eq!(r.write, want & Rights::WRITE.bits() as u64 != 0);
            }
            None => {
                // Solo se rechaza lo que no es READ o READ|WRITE.
                let r = Rights::READ.bits() as u64;
                assert!(want != r && want != r | Rights::WRITE.bits() as u64);
            }
        }
    }

    #[kani::proof]
    fn user_range_stays_low() {
        let (vaddr, size, off): (u64, u64, u64) = (kani::any(), kani::any(), kani::any());
        if user_range_ok(vaddr, size, crate::elf::USER_MIN, crate::elf::USER_MAX) && off < size {
            let v = vaddr + off;
            assert!(v >= crate::elf::USER_MIN && v < crate::elf::USER_MAX);
            assert!(index(v, 4) < KERNEL_HALF);
        }
    }
}
