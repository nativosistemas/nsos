//! Lector de device tree aplanado (FDT, el formato `.dtb`).
//!
//! En ARM64 el firmware describe la máquina (dónde están el UART, el GIC,
//! el controlador PCI...) en un device tree que Limine entrega al kernel.
//! Esos bytes **no son de confianza**: un DTB roto o malicioso no puede
//! hacer que el kernel entre en pánico, lea fuera del blob ni se cuelgue.
//! Por eso este módulo:
//!
//! * solo usa índices verificados (`get`, `checked_add`): ninguna operación
//!   puede entrar en pánico, sea cual sea el contenido (Kani lo demuestra,
//!   ver `verification/kani.md`);
//! * avanza al menos 4 bytes por token, así que todo recorrido termina;
//! * limita la profundidad a [`MAX_DEPTH`] (un árbol más hondo se corta).
//!
//! Lo que devuelve son *datos*: direcciones que el kernel todavía tiene que
//! validar (ver [`crate::platform`]) antes de mapearlas.
//!
//! Referencia: Devicetree Specification v0.4, capítulo 5.

/// Magia de la cabecera (`0xd00dfeed`, big-endian).
pub const MAGIC: u32 = 0xd00d_feed;
/// Tamaño de la cabecera de un FDT versión 17.
pub const HEADER_LEN: usize = 40;
/// Profundidad máxima que se recorre (la raíz es 0).
pub const MAX_DEPTH: usize = 12;

const TOKEN_BEGIN_NODE: u32 = 1;
const TOKEN_END_NODE: u32 = 2;
const TOKEN_PROP: u32 = 3;
const TOKEN_NOP: u32 = 4;
const TOKEN_END: u32 = 9;

/// Errores al abrir un blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FdtError {
    /// Más corto que la cabecera o que `totalsize`.
    Truncated,
    /// No empieza con [`MAGIC`].
    BadMagic,
    /// Versión incompatible (se necesita compatibilidad con la 16 o 17).
    BadVersion,
    /// Los bloques de estructura o de cadenas caen fuera del blob.
    BadLayout,
}

fn be32(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off.checked_add(4)?)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// Lee `cells` celdas de 32 bits (big-endian) como un número. Más de dos
/// celdas no entran en 64 bits: `None`.
pub fn read_cells(b: &[u8], cells: u32) -> Option<u64> {
    match cells {
        0 => Some(0),
        1 => be32(b, 0).map(u64::from),
        2 => Some((u64::from(be32(b, 0)?) << 32) | u64::from(be32(b, 4)?)),
        _ => None,
    }
}

fn align4(v: usize) -> Option<usize> {
    Some(v.checked_add(3)? & !3)
}

/// Un device tree validado en lo mínimo (cabecera y bloques dentro del blob).
#[derive(Clone, Copy)]
pub struct Fdt<'a> {
    structs: &'a [u8],
    strings: &'a [u8],
}

/// Token del bloque de estructura.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token<'a> {
    Begin(&'a [u8]),
    End,
    Prop(&'a [u8], &'a [u8]),
}

impl<'a> Fdt<'a> {
    /// Tamaño total que declara la cabecera (para saber cuánto copiar antes
    /// de validar). `None` si no hay cabecera.
    pub fn total_size(header: &[u8]) -> Option<usize> {
        if be32(header, 0)? != MAGIC {
            return None;
        }
        be32(header, 4).map(|v| v as usize)
    }

    /// Valida la cabecera y los límites de los bloques.
    pub fn new(blob: &'a [u8]) -> Result<Self, FdtError> {
        if blob.len() < HEADER_LEN {
            return Err(FdtError::Truncated);
        }
        let h = |i: usize| be32(blob, 4 * i).ok_or(FdtError::Truncated);
        if h(0)? != MAGIC {
            return Err(FdtError::BadMagic);
        }
        let total = h(1)? as usize;
        let blob = blob.get(..total).ok_or(FdtError::Truncated)?;
        let (off_struct, off_strings) = (h(2)? as usize, h(3)? as usize);
        let (version, last_comp) = (h(5)?, h(6)?);
        if version < 16 || last_comp > 17 {
            return Err(FdtError::BadVersion);
        }
        let (size_strings, size_struct) = (h(8)? as usize, h(9)? as usize);
        let slice = |off: usize, len: usize| blob.get(off..off.checked_add(len)?);
        let structs = slice(off_struct, size_struct).ok_or(FdtError::BadLayout)?;
        let strings = slice(off_strings, size_strings).ok_or(FdtError::BadLayout)?;
        if !off_struct.is_multiple_of(4) {
            return Err(FdtError::BadLayout);
        }
        Ok(Fdt { structs, strings })
    }

    /// Cadena terminada en NUL en el bloque de cadenas.
    fn string(&self, off: u32) -> Option<&'a [u8]> {
        let rest = self.strings.get(off as usize..)?;
        let end = rest.iter().position(|&c| c == 0)?;
        rest.get(..end)
    }

    /// Lee el token en `off`; devuelve el token y dónde empieza el próximo
    /// (siempre al menos 4 bytes más adelante). `None` al final o si el
    /// bloque está roto.
    fn token(&self, mut off: usize) -> Option<(Token<'a>, usize)> {
        loop {
            let tag = be32(self.structs, off)?;
            off = off.checked_add(4)?;
            match tag {
                TOKEN_NOP => continue,
                TOKEN_BEGIN_NODE => {
                    let rest = self.structs.get(off..)?;
                    let len = rest.iter().position(|&c| c == 0)?;
                    let name = rest.get(..len)?;
                    let next = align4(off.checked_add(len)?.checked_add(1)?)?;
                    return Some((Token::Begin(name), next));
                }
                TOKEN_END_NODE => return Some((Token::End, off)),
                TOKEN_PROP => {
                    let len = be32(self.structs, off)? as usize;
                    let nameoff = be32(self.structs, off.checked_add(4)?)?;
                    let start = off.checked_add(8)?;
                    let value = self.structs.get(start..start.checked_add(len)?)?;
                    let name = self.string(nameoff)?;
                    let next = align4(start.checked_add(len)?)?;
                    return Some((Token::Prop(name, value), next));
                }
                TOKEN_END => return None,
                // Un token desconocido también termina el recorrido.
                _ => return None,
            }
        }
    }

    /// Recorre todos los nodos en orden (profundidad primero), con la
    /// información de sus ancestros para leer `reg` y traducir direcciones.
    pub fn nodes(&self) -> Nodes<'a> {
        Nodes {
            fdt: *self,
            off: 0,
            depth: 0,
            stack: [Frame::ROOT_PARENT; MAX_DEPTH],
            done: false,
        }
    }

    /// El nodo raíz.
    pub fn root(&self) -> Option<Node<'a>> {
        self.nodes().next().filter(|n| n.depth == 0)
    }

    /// Primer nodo cuyo `compatible` incluye `compat`.
    pub fn find_compatible(&self, compat: &str) -> Option<Node<'a>> {
        self.nodes().find(|n| n.is_compatible(compat))
    }

    /// Nodo por ruta absoluta (`/soc/serial@7e201000`). Un componente sin
    /// `@` también acepta un nodo con dirección (`/chosen`, `/soc`). Si la
    /// ruta no empieza con `/`, se busca como alias en `/aliases`.
    pub fn find_path(&self, path: &[u8]) -> Option<Node<'a>> {
        let path = match path.iter().position(|&c| c == b':') {
            Some(i) => path.get(..i)?,
            None => path,
        };
        if path.first() != Some(&b'/') {
            let aliases = self.find_path(b"/aliases")?;
            let target = aliases.prop(path)?;
            let target = target.split(|&c| c == 0).next()?;
            if target.first() != Some(&b'/') {
                return None;
            }
            return self.find_path(target);
        }
        let mut want = [&[][..]; MAX_DEPTH];
        let mut n = 0;
        for comp in path.split(|&c| c == b'/').filter(|c| !c.is_empty()) {
            *want.get_mut(n)? = comp;
            n += 1;
        }
        // `matched` = cuántos componentes coinciden con la rama actual.
        let mut matched = 0usize;
        for node in self.nodes() {
            let d = node.depth;
            if d == 0 {
                if n == 0 {
                    return Some(node);
                }
                continue;
            }
            if d > matched + 1 {
                continue;
            }
            matched = d - 1;
            let comp = want.get(d - 1).copied().unwrap_or(&[]);
            if d <= n && name_matches(node.name, comp) {
                matched = d;
                if d == n {
                    return Some(node);
                }
            }
        }
        None
    }
}

fn name_matches(name: &[u8], comp: &[u8]) -> bool {
    if name == comp {
        return true;
    }
    !comp.contains(&b'@') && name.split(|&c| c == b'@').next() == Some(comp)
}

/// Lo que hace falta recordar de un nodo abierto para sus descendientes.
#[derive(Clone, Copy, Debug)]
struct Frame {
    /// Dónde empiezan sus propiedades en el bloque de estructura.
    props: usize,
    /// Su `#address-cells` (vale para sus hijos).
    addr_cells: u32,
    /// Su `#size-cells` (vale para sus hijos).
    size_cells: u32,
}

impl Frame {
    /// Valores por defecto de la especificación para la raíz.
    const ROOT_PARENT: Frame = Frame {
        props: usize::MAX,
        addr_cells: 2,
        size_cells: 1,
    };
}

/// Iterador sobre los nodos.
pub struct Nodes<'a> {
    fdt: Fdt<'a>,
    off: usize,
    depth: usize,
    stack: [Frame; MAX_DEPTH],
    done: bool,
}

impl<'a> Iterator for Nodes<'a> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Node<'a>> {
        while !self.done {
            let Some((tok, next)) = self.fdt.token(self.off) else {
                self.done = true;
                break;
            };
            self.off = next;
            match tok {
                Token::Begin(name) => {
                    let depth = self.depth;
                    if depth >= MAX_DEPTH {
                        // Demasiado hondo: se termina el recorrido.
                        self.done = true;
                        break;
                    }
                    let mut frame = Frame {
                        props: next,
                        addr_cells: 2,
                        size_cells: 1,
                    };
                    let mut p = next;
                    while let Some((Token::Prop(pname, value), after)) = self.fdt.token(p) {
                        match pname {
                            b"#address-cells" => {
                                frame.addr_cells = read_cells(value, 1).unwrap_or(2) as u32
                            }
                            b"#size-cells" => {
                                frame.size_cells = read_cells(value, 1).unwrap_or(1) as u32
                            }
                            _ => {}
                        }
                        p = after;
                    }
                    self.stack[depth] = frame;
                    self.depth = depth + 1;
                    let mut ancestors = [Frame::ROOT_PARENT; MAX_DEPTH];
                    ancestors[..depth].copy_from_slice(&self.stack[..depth]);
                    return Some(Node {
                        fdt: self.fdt,
                        name,
                        depth,
                        own: frame,
                        ancestors,
                    });
                }
                Token::End => {
                    if self.depth == 0 {
                        self.done = true;
                    } else {
                        self.depth -= 1;
                    }
                }
                Token::Prop(..) => {}
            }
        }
        None
    }
}

/// Un nodo del árbol.
#[derive(Clone, Copy)]
pub struct Node<'a> {
    fdt: Fdt<'a>,
    /// Nombre (`pl011@9000000`; vacío en la raíz).
    pub name: &'a [u8],
    /// Profundidad (la raíz es 0).
    pub depth: usize,
    own: Frame,
    /// `ancestors[i]` es el ancestro de profundidad `i` (`i < depth`).
    ancestors: [Frame; MAX_DEPTH],
}

impl<'a> Node<'a> {
    fn props_from(&self, start: usize) -> impl Iterator<Item = (&'a [u8], &'a [u8])> + 'a {
        let fdt = self.fdt;
        let mut off = Some(start);
        core::iter::from_fn(move || match fdt.token(off?) {
            Some((Token::Prop(n, v), next)) => {
                off = Some(next);
                Some((n, v))
            }
            _ => {
                off = None;
                None
            }
        })
    }

    /// Propiedades del nodo (nombre, valor).
    pub fn props(&self) -> impl Iterator<Item = (&'a [u8], &'a [u8])> + 'a {
        self.props_from(self.own.props)
    }

    /// Valor de la propiedad `name`.
    pub fn prop(&self, name: &[u8]) -> Option<&'a [u8]> {
        self.props().find(|(n, _)| *n == name).map(|(_, v)| v)
    }

    /// ¿La lista `compatible` incluye `compat`?
    pub fn is_compatible(&self, compat: &str) -> bool {
        self.prop(b"compatible").is_some_and(|v| {
            v.split(|&c| c == 0)
                .any(|s| !s.is_empty() && s == compat.as_bytes())
        })
    }

    /// `#address-cells` y `#size-cells` del padre (con los que se lee el
    /// `reg` de este nodo).
    pub fn parent_cells(&self) -> (u32, u32) {
        match self.depth.checked_sub(1) {
            Some(p) => self
                .ancestors
                .get(p)
                .map_or((2, 1), |f| (f.addr_cells, f.size_cells)),
            None => (2, 1),
        }
    }

    /// `#address-cells` y `#size-cells` propios (para leer `ranges` y los
    /// `reg` de sus hijos).
    pub fn own_cells(&self) -> (u32, u32) {
        (self.own.addr_cells, self.own.size_cells)
    }

    /// Entrada `i` de `reg` (dirección en el espacio del padre, tamaño).
    /// `None` si no existe o no entra en 64 bits.
    pub fn reg_raw(&self, i: usize) -> Option<(u64, u64)> {
        let (ac, sc) = self.parent_cells();
        reg_entry(self.prop(b"reg")?, ac, sc, i)
    }

    /// Entrada `i` de `reg` traducida a una dirección física de la CPU (a
    /// través de los `ranges` de los ancestros). `None` si algún ancestro
    /// no la traduce o si la región entera no cabe en una ventana.
    pub fn reg(&self, i: usize) -> Option<(u64, u64)> {
        let (addr, size) = self.reg_raw(i)?;
        let mut addr = addr;
        // El padre de profundidad p traduce del espacio de sus hijos al
        // de su propio padre. La raíz (p = 0) no traduce.
        let mut p = self.depth.checked_sub(1)?;
        while p > 0 {
            let bus = *self.ancestors.get(p)?;
            let up = *self.ancestors.get(p - 1)?;
            addr = translate(self, bus, up.addr_cells, addr, size)?;
            p -= 1;
        }
        Some((addr, size))
    }
}

/// Entrada `i` de un `reg` con `ac` celdas de dirección y `sc` de tamaño.
fn reg_entry(reg: &[u8], ac: u32, sc: u32, i: usize) -> Option<(u64, u64)> {
    let entry = 4 * (ac as usize).checked_add(sc as usize)?;
    if entry == 0 {
        return None;
    }
    let start = i.checked_mul(entry)?;
    let e = reg.get(start..start.checked_add(entry)?)?;
    let addr = read_cells(e, ac)?;
    let size = read_cells(e.get(4 * ac as usize..)?, sc)?;
    Some((addr, size))
}

/// Traduce `[addr, addr + size)` con los `ranges` de `bus`.
fn translate(node: &Node<'_>, bus: Frame, parent_ac: u32, addr: u64, size: u64) -> Option<u64> {
    let mut ranges = None;
    for (n, v) in node.props_from(bus.props) {
        if n == b"ranges" {
            ranges = Some(v);
            break;
        }
    }
    // Sin `ranges`: el bus no es traducible. `ranges` vacío: identidad.
    translate_ranges(
        ranges?,
        bus.addr_cells,
        parent_ac,
        bus.size_cells,
        addr,
        size,
    )
}

/// Busca en `ranges` (entradas de `cac` celdas del hijo, `pac` del padre y
/// `sc` de tamaño) una ventana que contenga `[addr, addr + size)` entera y
/// devuelve la dirección del lado del padre.
fn translate_ranges(
    ranges: &[u8],
    cac: u32,
    pac: u32,
    sc: u32,
    addr: u64,
    size: u64,
) -> Option<u64> {
    if ranges.is_empty() {
        return Some(addr);
    }
    let entry = 4
        * (cac as usize)
            .checked_add(pac as usize)?
            .checked_add(sc as usize)?;
    if entry == 0 {
        return None;
    }
    let end = addr.checked_add(size)?;
    for e in ranges.chunks_exact(entry) {
        let child = read_cells(e, cac)?;
        let parent = read_cells(e.get(4 * cac as usize..)?, pac)?;
        let len = read_cells(e.get(4 * (cac as usize + pac as usize)..)?, sc)?;
        let Some(child_end) = child.checked_add(len) else {
            continue;
        };
        if addr >= child && end <= child_end {
            return parent.checked_add(addr - child);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Arma un FDT a mano: `f` agrega tokens con el `Builder`.
    pub(super) struct Builder {
        st: Vec<u8>,
        strings: Vec<u8>,
    }

    impl Builder {
        pub(super) fn new() -> Self {
            Builder {
                st: Vec::new(),
                strings: Vec::new(),
            }
        }
        fn pad(&mut self) {
            while !self.st.len().is_multiple_of(4) {
                self.st.push(0);
            }
        }
        pub(super) fn begin(&mut self, name: &str) -> &mut Self {
            self.st.extend_from_slice(&TOKEN_BEGIN_NODE.to_be_bytes());
            self.st.extend_from_slice(name.as_bytes());
            self.st.push(0);
            self.pad();
            self
        }
        pub(super) fn end(&mut self) -> &mut Self {
            self.st.extend_from_slice(&TOKEN_END_NODE.to_be_bytes());
            self
        }
        pub(super) fn prop(&mut self, name: &str, value: &[u8]) -> &mut Self {
            let off = self.strings.len() as u32;
            self.strings.extend_from_slice(name.as_bytes());
            self.strings.push(0);
            self.st.extend_from_slice(&TOKEN_PROP.to_be_bytes());
            self.st
                .extend_from_slice(&(value.len() as u32).to_be_bytes());
            self.st.extend_from_slice(&off.to_be_bytes());
            self.st.extend_from_slice(value);
            self.pad();
            self
        }
        pub(super) fn cells(&mut self, name: &str, cells: &[u32]) -> &mut Self {
            let v: Vec<u8> = cells.iter().flat_map(|c| c.to_be_bytes()).collect();
            self.prop(name, &v)
        }
        pub(super) fn build(&mut self) -> Vec<u8> {
            self.st.extend_from_slice(&TOKEN_END.to_be_bytes());
            let off_struct = HEADER_LEN + 16; // mapa de reservas vacío
            let off_strings = off_struct + self.st.len();
            let total = off_strings + self.strings.len();
            let mut b = Vec::new();
            for v in [
                MAGIC,
                total as u32,
                off_struct as u32,
                off_strings as u32,
                HEADER_LEN as u32,
                17,
                16,
                0,
                self.strings.len() as u32,
                self.st.len() as u32,
            ] {
                b.extend_from_slice(&v.to_be_bytes());
            }
            b.extend_from_slice(&[0; 16]);
            b.extend_from_slice(&self.st);
            b.extend_from_slice(&self.strings);
            b
        }
    }

    /// Un árbol como el de una Raspberry Pi 4: los periféricos cuelgan de
    /// `/soc`, que traduce 0x7e000000 (bus) a 0xfe000000 (CPU).
    pub(super) fn pi_like() -> Vec<u8> {
        Builder::new()
            .begin("")
            .cells("#address-cells", &[2])
            .cells("#size-cells", &[1])
            .begin("aliases")
            .prop("serial0", b"/soc/serial@7e201000\0")
            .end()
            .begin("chosen")
            .prop("stdout-path", b"serial0:115200n8\0")
            .prop("rng-seed", &[7; 32])
            .end()
            .begin("soc")
            .prop("compatible", b"simple-bus\0")
            .cells("#address-cells", &[1])
            .cells("#size-cells", &[1])
            .cells("ranges", &[0x7e00_0000, 0, 0xfe00_0000, 0x0180_0000])
            .begin("serial@7e201000")
            .prop("compatible", b"arm,pl011\0arm,primecell\0")
            .cells("reg", &[0x7e20_1000, 0x200])
            .end()
            .begin("noranges")
            .cells("#address-cells", &[1])
            .cells("#size-cells", &[1])
            .begin("dev@10")
            .cells("reg", &[0x10, 0x10])
            .end()
            .end()
            .end()
            .begin("interrupt-controller@40041000")
            .prop("compatible", b"arm,gic-400\0")
            .cells("reg", &[0, 0xff84_1000, 0x1000, 0, 0xff84_2000, 0x2000])
            .end()
            .end()
            .build()
    }

    #[test]
    fn header_errors() {
        assert_eq!(Fdt::new(&[]).err(), Some(FdtError::Truncated));
        let mut b = pi_like();
        b[0] = 0;
        assert_eq!(Fdt::new(&b).err(), Some(FdtError::BadMagic));
        let mut b = pi_like();
        b[23] = 2; // versión 2
        assert_eq!(Fdt::new(&b).err(), Some(FdtError::BadVersion));
        let mut b = pi_like();
        b[39] = 0xff; // tamaño del bloque de estructura gigante
        assert_eq!(Fdt::new(&b).err(), Some(FdtError::BadLayout));
        let b = pi_like();
        assert_eq!(Fdt::new(&b[..b.len() - 1]).err(), Some(FdtError::Truncated));
    }

    #[test]
    fn translates_through_soc() {
        let b = pi_like();
        let fdt = Fdt::new(&b).unwrap();
        let uart = fdt.find_compatible("arm,pl011").unwrap();
        assert_eq!(uart.name, b"serial@7e201000");
        assert_eq!(uart.depth, 2);
        assert_eq!(uart.reg_raw(0), Some((0x7e20_1000, 0x200)));
        assert_eq!(uart.reg(0), Some((0xfe20_1000, 0x200)));
        assert_eq!(uart.reg(1), None);
        let gic = fdt.find_compatible("arm,gic-400").unwrap();
        assert_eq!(gic.reg(0), Some((0xff84_1000, 0x1000)));
        assert_eq!(gic.reg(1), Some((0xff84_2000, 0x2000)));
        // Un bus sin `ranges` no es traducible.
        let dev = fdt.find_path(b"/soc/noranges/dev@10").unwrap();
        assert_eq!(dev.reg_raw(0), Some((0x10, 0x10)));
        assert_eq!(dev.reg(0), None);
    }

    #[test]
    fn paths_and_aliases() {
        let b = pi_like();
        let fdt = Fdt::new(&b).unwrap();
        assert_eq!(fdt.find_path(b"/").unwrap().depth, 0);
        let chosen = fdt.find_path(b"/chosen").unwrap();
        assert_eq!(chosen.prop(b"rng-seed"), Some(&[7u8; 32][..]));
        let out = chosen.prop(b"stdout-path").unwrap();
        let uart = fdt.find_path(&out[..out.len() - 1]).unwrap();
        assert_eq!(uart.name, b"serial@7e201000");
        assert!(fdt.find_path(b"/soc/serial").is_some());
        assert!(fdt.find_path(b"/soc/serial@7e201001").is_none());
        assert!(fdt.find_path(b"/serial@7e201000").is_none());
        assert!(fdt.find_path(b"/interrupt-controller").is_some());
        assert!(fdt.find_path(b"serial9").is_none());
    }

    /// Cualquier corte o byte cambiado: sin pánicos (lo complementa Kani).
    #[test]
    fn corrupted_never_panics() {
        let b = pi_like();
        let walk = |b: &[u8]| {
            if let Ok(f) = Fdt::new(b) {
                for n in f.nodes() {
                    for (_, _) in n.props() {}
                    let _ = (n.reg(0), n.reg(1), n.is_compatible("arm,pl011"));
                }
                let _ = f.find_path(b"/soc/serial@7e201000");
                let _ = f.find_path(b"serial0");
            }
        };
        for i in 0..b.len() {
            for v in [0u8, 1, 2, 3, 4, 9, 0x7f, 0xff] {
                let mut c = b.clone();
                c[i] = v;
                walk(&c);
            }
            walk(&b[..i]);
        }
    }

    /// Miles de árboles con varios bytes cambiados al azar (también sobre
    /// el DTB real de QEMU): sin pánicos y todo recorrido termina. Cubre
    /// el iterador de nodos, que es demasiado grande para Kani (ver
    /// `verification/kani.md`).
    #[test]
    fn random_corruption_never_panics() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let qemu = include_bytes!("../tests/data/qemu-virt.dtb").to_vec();
        for base in [pi_like(), qemu] {
            for _ in 0..3000 {
                let mut c = base.clone();
                for _ in 0..1 + rnd() % 8 {
                    let i = (rnd() as usize) % c.len();
                    c[i] = rnd() as u8;
                }
                let Ok(f) = Fdt::new(&c) else { continue };
                let mut steps = 0usize;
                for n in f.nodes() {
                    steps += 1;
                    assert!(n.depth < MAX_DEPTH && steps <= c.len() / 4);
                    for _ in n.props() {}
                    let _ = (n.reg(0), n.reg(1), n.own_cells());
                }
                let _ = crate::platform::Platform::from_fdt(&f);
            }
        }
    }

    /// El DTB real de QEMU `virt` (generado con `-M virt,dumpdtb=`).
    #[test]
    fn qemu_virt_dtb() {
        let b = include_bytes!("../tests/data/qemu-virt.dtb");
        let fdt = Fdt::new(b).unwrap();
        let uart = fdt.find_compatible("arm,pl011").unwrap();
        assert_eq!(uart.reg(0), Some((0x0900_0000, 0x1000)));
        let gic = fdt.find_compatible("arm,cortex-a15-gic").unwrap();
        assert_eq!(gic.reg(0), Some((0x0800_0000, 0x1_0000)));
        assert_eq!(gic.reg(1), Some((0x0801_0000, 0x1_0000)));
        let pci = fdt.find_compatible("pci-host-ecam-generic").unwrap();
        assert_eq!(pci.reg(0), Some((0x40_1000_0000, 0x1000_0000)));
        assert_eq!(pci.own_cells(), (3, 2));
        let chosen = fdt.find_path(b"/chosen").unwrap();
        assert_eq!(chosen.prop(b"rng-seed").map(|s| s.len()), Some(32));
        let out = chosen.prop(b"stdout-path").unwrap();
        let out = out.split(|&c| c == 0).next().unwrap();
        assert_eq!(fdt.find_path(out).unwrap().name, b"pl011@9000000");
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Bloques de estructura y de cadenas cualesquiera, chicos.
    fn any_fdt<'a>(st: &'a [u8], strings: &'a [u8]) -> Fdt<'a> {
        let (a, b): (usize, usize) = (kani::any(), kani::any());
        kani::assume(a <= st.len() && b <= strings.len());
        Fdt {
            structs: &st[..a],
            strings: &strings[..b],
        }
    }

    /// El corazón del lector: leer un token en cualquier posición de
    /// cualquier bloque nunca entra en pánico, y si devuelve algo, el
    /// próximo token empieza al menos 4 bytes más adelante. Por eso todo
    /// recorrido (`nodes`, `props`, `find_path`) termina.
    #[kani::proof]
    #[kani::unwind(10)]
    fn token_any() {
        let st: [u8; 8] = kani::any();
        let strings: [u8; 2] = kani::any();
        let fdt = any_fdt(&st, &strings);
        // Más allá del bloque, `be32` devuelve `None` (con `get`).
        let off: usize = kani::any();
        kani::assume(off <= st.len() + 4);
        if let Some((tok, next)) = fdt.token(off) {
            assert!(next >= off + 4 && next <= fdt.structs.len() + 3);
            if let Token::Prop(name, _) = tok {
                assert!(name.len() < 2);
            }
        }
    }

    /// Abrir cualquier blob (cabecera y algo más, contenido cualquiera):
    /// sin pánicos; si se acepta, los bloques están dentro del blob.
    #[kani::proof]
    fn new_any() {
        let bytes: [u8; HEADER_LEN + 4] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= bytes.len());
        if let Ok(f) = Fdt::new(&bytes[..len]) {
            assert!(f.structs.len() <= len && f.strings.len() <= len);
        }
    }

    /// Leer la entrada `i` de un `reg` cualquiera, con cualquier cantidad
    /// de celdas: sin pánicos ni desbordes.
    #[kani::proof]
    #[kani::unwind(4)]
    fn reg_entry_any() {
        let b: [u8; 16] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= b.len());
        let _ = reg_entry(&b[..len], kani::any(), kani::any(), kani::any());
    }

    /// Traducir por un `ranges` cualquiera, con cualquier cantidad de
    /// celdas y cualquier región: sin pánicos ni desbordes; si traduce, la
    /// región entera cabe en una ventana y la dirección no desborda.
    #[kani::proof]
    #[kani::unwind(8)]
    fn translate_any() {
        let b: [u8; 24] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= b.len());
        let (cac, pac, sc): (u32, u32, u32) = (kani::any(), kani::any(), kani::any());
        let (addr, size): (u64, u64) = (kani::any(), kani::any());
        if let Some(_p) = translate_ranges(&b[..len], cac, pac, sc, addr, size) {
            assert!(len == 0 || addr.checked_add(size).is_some());
        }
    }

    /// `read_cells` con cualquier entrada.
    #[kani::proof]
    fn read_cells_any() {
        let b: [u8; 8] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= 8);
        let cells: u32 = kani::any();
        if let Some(v) = read_cells(&b[..len], cells) {
            assert!(cells <= 2 && len >= 4 * cells as usize);
            if cells == 1 {
                assert!(v <= u32::MAX as u64);
            }
        }
    }
}
